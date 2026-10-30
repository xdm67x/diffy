//! The app's HTTP client, so remote media in PR descriptions (images, video
//! posters) loads over the network. On macOS it is `NSURLSession`; other
//! platforms keep GPUI's failing default client.

use gpui_kit::App;

/// Installs the app's HTTP client. Call once during startup.
pub fn install(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.set_http_client(std::sync::Arc::new(macos::NsUrlSessionClient));
    #[cfg(not(target_os = "macos"))]
    let _ = cx;
}

#[cfg(target_os = "macos")]
mod macos {
    use block2::RcBlock;
    use futures::{AsyncReadExt as _, future::BoxFuture};
    use gpui_kit::http_client::{
        AsyncBody, HttpClient, Request, Response, Result, Url, anyhow,
        http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Version, request::Parts},
    };
    use objc2::{ClassType as _, msg_send, rc::Retained};
    use objc2_foundation::{
        NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSString, NSURL, NSURLResponse,
        NSURLSession,
    };

    /// An [`HttpClient`] backed by the shared `NSURLSession`.
    pub struct NsUrlSessionClient;

    impl HttpClient for NsUrlSessionClient {
        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(&self, req: Request<AsyncBody>) -> BoxFuture<'static, Result<Response<AsyncBody>>> {
            Box::pin(async move {
                let (parts, mut body) = req.into_parts();
                let mut body_bytes = Vec::new();
                body.read_to_end(&mut body_bytes).await?;

                // All Objective-C objects are created and dropped inside
                // `start_request`, before the only await below, so the
                // returned future stays `Send`.
                let (tx, rx) = futures::channel::oneshot::channel();
                start_request(parts, body_bytes, tx);
                rx.await
                    .map_err(|_| anyhow!("the request was dropped before completing"))?
            })
        }
    }

    /// Builds the `NSURLRequest`, starts the data task, and hands the
    /// completion through `tx`.
    fn start_request(
        parts: Parts,
        body_bytes: Vec<u8>,
        tx: futures::channel::oneshot::Sender<Result<Response<AsyncBody>>>,
    ) {
        let Some(ns_url) = NSURL::URLWithString(&NSString::from_str(&parts.uri.to_string())) else {
            let _ = tx.send(Err(anyhow!("invalid URL: {}", parts.uri)));
            return;
        };
        let request = NSMutableURLRequest::requestWithURL(&ns_url);
        request.setHTTPMethod(&NSString::from_str(parts.method.as_str()));
        for (name, value) in parts.headers.iter() {
            let Some(value) = value.to_str().ok() else {
                continue;
            };
            request.setValue_forHTTPHeaderField(
                Some(&NSString::from_str(value)),
                &NSString::from_str(name.as_str()),
            );
        }
        if !body_bytes.is_empty() {
            request.setHTTPBody(Some(&NSData::from_vec(body_bytes)));
        }

        // `oneshot::Sender::send` is `FnOnce`; the completion handler may run
        // more than once in theory, so guard the sender behind a mutex.
        let tx = std::sync::Mutex::new(Some(tx));
        let handler = RcBlock::new(
            move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(finish(data, response, error));
                }
            },
        );
        // SAFETY: the call copies the block, so dropping `handler` after
        // `resume` does not invalidate the task's copy.
        let task = unsafe {
            NSURLSession::sharedSession().dataTaskWithRequest_completionHandler(&request, &handler)
        };
        task.resume();
    }

    /// Turns the completion handler's arguments into an `http::Response`.
    fn finish(
        data: *mut NSData,
        response: *mut NSURLResponse,
        error: *mut NSError,
    ) -> Result<Response<AsyncBody>> {
        if !error.is_null() {
            // SAFETY: the completion handler guarantees a valid object.
            let error = unsafe { &*error };
            return Err(anyhow!("request failed: {}", error.localizedDescription()));
        }
        // SAFETY: the completion handler guarantees a valid object.
        let Some(response) = (unsafe { response.as_ref() }) else {
            return Err(anyhow!("the server returned no response"));
        };
        let mut builder = Response::builder().version(Version::HTTP_11);
        let mut headers = HeaderMap::new();
        // SAFETY: `NSHTTPURLResponse` subclasses `NSURLResponse`, so the cast
        // is sound once `isKindOfClass` confirms it.
        if unsafe { msg_send![response, isKindOfClass: NSHTTPURLResponse::class()] } {
            let http = unsafe { &*(response as *const NSURLResponse as *const NSHTTPURLResponse) };
            if let Ok(status) = StatusCode::from_u16(http.statusCode() as u16) {
                builder = builder.status(status);
            }
            let fields = http.allHeaderFields();
            // SAFETY: `keyEnumerator` returns the dictionary's keys.
            let keys = unsafe { fields.keyEnumerator() };
            while let Some(key) = keys.nextObject() {
                let Some(value) = fields.objectForKey(&key) else {
                    continue;
                };
                // SAFETY: `description` answers an `NSString` for any NSObject.
                let key: Retained<NSString> = unsafe { msg_send![&*key, description] };
                let value: Retained<NSString> = unsafe { msg_send![&*value, description] };
                let (key, value) = (key.to_string(), value.to_string());
                if let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(key.as_bytes()),
                    HeaderValue::from_str(&value),
                ) {
                    headers.insert(name, value);
                }
            }
        }
        if let Some(builder_headers) = builder.headers_mut() {
            *builder_headers = headers;
        }
        let body = if data.is_null() {
            Vec::new()
        } else {
            // SAFETY: the completion handler guarantees a valid object.
            unsafe { &*data }.to_vec()
        };
        Ok(builder.body(AsyncBody::from(body))?)
    }
}
