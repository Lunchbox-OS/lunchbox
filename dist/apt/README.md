# apt repository

`repository.key` belongs here: the public half of the Lunchbox archive signing
key, which signs the apt repository at <https://apt.lunchbox-os.com> and the
`.asc` beside each released `.deb`. It is committed so that a change to the key
users trust shows up in a diff, and it is what the release deploys as
`https://apt.lunchbox-os.com/repository.key`.

Primary `4AD4 9057 5B0A 5535 2C03  BD7A C37C 23C7 CE3B 9618`, signing subkey
`FC49 384A 3C18 2B32 C37D  D732 386B 3B92 ECDC D3DA`, both Ed25519 with no
expiry. How it was made, where the private halves live, and what to do if it
leaks are in [docs/release-signing.md](../../docs/release-signing.md). Replacing
this file is a key rotation, and every installed device has to fetch the new
one by hand.

The repository is built by `scripts/ci/publish-apt.sh`. Nothing else in this
directory is published.
