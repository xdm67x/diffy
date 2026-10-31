---
name: code-simplifier
description: Make existing code readable for a human reader — rename variables, functions, types and enums so their names say what they do, and delete comments that only restate the code. Behavior-preserving cleanup in any language. Use when asked to simplify, clean up, clarify, rename, de-comment or review code for readability, and after a change leaves names or comments that no longer match what the code does.
---

# Code simplifier

The reader is a human who arrives later, without the conversation that produced
the code. Your job is that they can follow it top to bottom. Names and comments
are most of that, so work only on names, comments and the small structural noise
that hides intent.

Hard rule: **behavior stays identical.** Same inputs, same outputs, same errors,
same ordering, same side effects. The formatter and the tests must be as green
afterwards as before. If you spot a real bug, stop, report it separately, and do
not fold the fix into the rename.

## Work in this order

1. **Read before touching.** Read the target file top to bottom, then its
   callers. Understand what each piece is for *at its call site* — a name's
   correct form is decided by the caller, not the definition.
2. **Find every use first.** Grep each identifier you plan to rename before
   renaming it: definitions, callers, imports, tests, fixtures, trait/interface
   implementations, reflection or string lookups. A rename that misses one use
   is a compile error at best, a silent bug at worst.
3. **Rename, then delete comments.** A good name deletes the comment that was
   explaining it. Do the names first, look at what comments became redundant.
4. **Run the project's build, lint and tests.** Keep formatting after the edits,
   not before.

## Naming

Name the thing's role in *this* program, not its type or shape.

- **Variables**: say what it holds and, when it matters, its unit or scope —
  `retryDelayMillis`, `openPullRequestCounts`, `unparsedReviewBody`. Avoid
  `data`, `info`, `value`, `result`, `tmp`, `obj`, `flag`, `n`, `i` unless the
  scope is three lines and the meaning is obvious (`for i in 0..len` over an
  index is fine).
- **Booleans**: read as a condition that is true or false — `isDirty`,
  `hasUncommittedChanges`, `shouldRetry`. Not `flag`, `check`, `status`.
- **Functions**: a verb phrase describing the effect, not the mechanism —
  `postReviewToGitHub`, not `handleSubmit` or `doProcess`. If the honest name
  needs "and", the function does two things; say so in the report rather than
  hiding it.
- **Types and structs**: the domain concept they model — `ReviewDraft`, not
  `ReviewData` or `ReviewModel`. Name what it *is* in the app's language.
- **Enums and variants**: singular concept for the enum (`JobState`,
  `Transport`), variants that name the state or alternative outright
  (`Pending`, `Failed { reason }`) — never `Variant1`, `Other`, `TypeA`, and
  avoid variants that only make sense with the enum name prefixed
  (`JobStatePending` inside `JobState`).
- **Generic parameters and callbacks**: name what the caller receives or
  supplies — `T` becomes `Item`/`Row`/`Message`, a closure's arguments get the
  names the body uses, not `a`, `b`.
- **Loops and matches**: name the element, not the container it came from
  (`for pr in pullRequests`, not `for pr in list`).
- **Consistency**: follow the spelling the codebase already uses for a concept
  (`pr` vs `pullRequest`, `repo` vs `repository`). A rename into your own
  favorite spelling is churn.

Say what it means, not what it currently equals. `let isLastPage = page == pages`
is a good name; `isThree` is not.

## Comments

Delete:

- comments restating the next line (`// increment counter` above `i += 1`);
- section banners and separator rules in short functions;
- commented-out code (git has it, and nobody trusts it);
- comments that describe *what used to be* there — the history is in the log;
- doc comments that repeat the signature in prose;
- `TODO` that names no owner and no condition.

Keep, and keep short:

- *why* the code is this way: an invariant, a platform quirk, an ordering that
  matters, a workaround with the bug link;
- warnings the code cannot express: don't reorder these two calls, this value is
  attacker controlled, this cast is safe because of the check above;
- one line of context that saves the reader the surrounding module — where the
  input comes from, what the caller is expected to do with the result.

Rules of thumb: if the comment can be replaced by a better name, replace it. If
it explains what the function does for callers, it belongs in the doc comment of
a public item, one line, in terms of the caller's problem. Never leave a comment
that is now wrong — a stale comment is worse than no comment.

## Structure, lightly

Only where it unblocks the names:

- Early return over nested `if`s, so the happy path reads straight down.
- A named boolean for a compound condition that a comment used to explain.
- Split a function that mixes levels (parsing, then deciding, then doing) *only
  if the split is obvious and small*; do not invent layers, traits, wrappers,
  options or config nobody asked for.
- Inline a single-use variable whose only job was to hold an expression under a
  misleading name.

## Do not touch

- Names that are part of a contract: serialized fields and their
  `rename`/`alias` attributes, JSON keys, database columns, HTTP routes and
  error codes, CLI flags, environment variables, file formats, public API of a
  published crate or library. Rename these only with a migration, and not as
  part of a readability pass.
- Behavior, types, control flow, error messages the user sees.
- Dependencies, file layout, formatting beyond the project's own formatter.
- Code you only renamed in your head: no "while I was here" refactors, and no
  second concern in the same commit.

## Verify

1. The project's build and lint pass; tests pass unchanged. If you had to change
   a test to make it compile, that is a signal to check the rename.
2. Read the diff's deleted lines: every one should be a name or a comment, plus
   the trivial lines the rename touched.
3. Grep the old identifiers one last time — including strings, docs and config —
   to confirm nothing still refers to them.

## Report

Short and factual: the renames that changed how the code reads, the comments you
deleted and what replaced them (usually a name), what you left alone and why, and
the line that behavior is unchanged with the commands you ran to prove it.
