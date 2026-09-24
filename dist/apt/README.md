# apt repository

`repository.key` belongs here: the public half of the Lunchbox archive signing
key, which signs the apt repository at <https://apt.lunchbox-os.com> and the
`.asc` beside each released `.deb`. It is committed so that a change to the key
users trust shows up in a diff, and it is what the release deploys as
`https://apt.lunchbox-os.com/repository.key`.

It is not here until the one-time key ceremony in
[docs/release-signing.md](../../docs/release-signing.md) has run. Until then a
release tag fails at its signing step, before anything is published.

The repository is built by `scripts/ci/publish-apt.sh`. Nothing else in this
directory is published.
