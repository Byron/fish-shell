# Project workflow

## Commit each semantic step

- End every coherent feature, behavior change, fix, or documentation milestone with a commit before starting the next semantic step.
- Include the implementation, relevant tests, and user-facing documentation for that step. Run appropriate checks before committing and record material validation limits.
- Use a Conventional Commit title: `type(optional-scope): describe the behavior`, for example `feat(windows): support background jobs`.
- Write a Markdown commit body that explains what users can do, why the change matters, and any remaining limitations. Emphasize the feature and its behavior more than implementation technicalities. Include a concise validation summary when relevant.
- Keep each commit focused. Stage only the completed step and preserve unrelated or unfinished work.
- When agents share a working tree, coordinate a commit boundary before staging so a commit does not include another agent's unfinished changes.

## Git integrations

- Implement Git repository integrations with the `gix` crate, not Git subprocesses.
- Gate repository integration behind the default-enabled `git` Cargo feature. Disabling the feature or encountering unsupported repository storage must not fall back to invoking Git.
