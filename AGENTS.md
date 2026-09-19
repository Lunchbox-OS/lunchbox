Issues are tracked on GitHub at <https://github.com/aarmea/lunchbox/issues>.

Issue numbers 1-201 predate this repository and kept the values they had on
Forgejo, where issues and pull requests shared one sequence. The numbers that
belonged to pull requests are held by closed placeholder issues labelled
`legacy-pull-request`, so `#N` still means what it always meant; those pull
requests' descriptions, CI results and discussion are archived at
<https://github.com/aarmea/lunchbox-legacy-issues-prs>. Filter with
`is:issue -label:legacy-pull-request` to see only real issues. See
<docs/ai/history/2026-09-19 001 forgejo-to-github-issue-migration.md>.

Agents: please use the existing documentation for setup.

<CONTRIBUTING.md> describes environment setup and build, test, and lint, including helper scripts and exact commands.

Please ensure that your changes build and pass tests and lint, and run `cargo fmt --all` to match your changes to the rest of the code.

To see or verify a UI change end-to-end without a graphical login session, drive
the headless dev session (`./scripts/lunchbox dev headless` → `dev shot` →
`dev stop`); see the `headless-dev` skill and the "Headless development" section
of <CONTRIBUTING.md>. Prefer this over `./run-dev`, which requires a login session.

If you changed the example configuration at <config.example.toml>, make sure that it passes config validation.

Each of the Rust crates in <crates> contains a README.md that describes each at a high level.

<.github/workflows/ci.yml> and <docs/INSTALL.md> describes exact environment setup, especially if coming from Ubuntu 24.04 (lunchbox-launcher requires 26.04).

Historical prompts and design docs provided to agents are placed in <docs/ai/history>. Please refer there for history, and if this prompt is substantial, write it along with any relevant context (like the GitHub issue) to that directory as well.

When you learn something durable and project-specific (a workflow quirk, a tooling gotcha, a non-obvious invariant), prefer writing it into in-repo documentation — this file, the relevant `.claude/skills/*/SKILL.md`, a crate `README.md`, `CONTRIBUTING.md`, or a <docs/ai/history> note — over your own private memory. In-repo docs are versioned, reviewable, and shared with every agent and human on the project; private memory is not.
