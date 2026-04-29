### Please disclose the use of AI

If AI edits files for you, disclose it in the PR description and commit metadata. Prefer making the
agent identity part of the commit, for example by using an AI *author* such as `$agent $version <ai-agent@example.invalid>`
or a *co-author* via `Co-authored-by: <agent-identity>` trailer.
Recent commits in this repository use that pattern, often with a *human* `Co-authored-by` trailer when a person also contributed directly.

Agents operating through a person's GitHub account must identify themselves. For example, comments
posted by an agent should say so directly with phrases like `AI agent on behalf of <person>: ...`.

Fully AI-generated comments on PRs or issues must also be disclosed. Undisclosed AI-generated
comments may lead to the PR or issue being closed.

AI-assisted proofreading or wording polish does not need disclosure, but it is still courteous to
mention it when the AI materially influenced the final text.

For everything else, please have a look at the respective section in the [README] file.

[README]: https://github.com/GitoxideLabs/gitoxide#contributions
