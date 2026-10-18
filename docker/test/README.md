# Patchbay integration-test SSH servers

> **TEST ONLY.** Everything in this directory - user keys in `keys/`, host keys in
> `hostkeys/`, the `patchbay`/`patchbay` password - is deliberately public and insecure.
> Never reuse any of it outside these throwaway containers.

```sh
docker compose -f docker/test/compose.yml up -d --build --wait   # start (healthchecked)
docker compose -f docker/test/compose.yml logs -f                # sshd logs (LogLevel VERBOSE)
docker compose -f docker/test/compose.yml down                   # stop
```

Integration tests in `patchbay-core` run only when `PATCHBAY_DOCKER_TESTS=1`.

## Contract

| Service    | Address           | Purpose |
|------------|-------------------|---------|
| `target`   | `127.0.0.1:2201`  | Full OpenSSH server: bash shell, exec, SFTP, forwarding, sample files |
| `bastion`  | `127.0.0.1:2202`  | Jump host; reaches `target:22` on the compose network |
| `sftponly` | `127.0.0.1:2203`  | `ForceCommand internal-sftp`, chroot, no exec/forwarding |

All three: user **`patchbay`**, password **`patchbay`**, uid 1000, and these keys in
`~/.ssh/authorized_keys`:

| File (in `keys/`)          | Type     | Passphrase | Authorized |
|----------------------------|----------|------------|------------|
| `id_ed25519`               | ed25519  | none       | yes |
| `id_ed25519_pass`          | ed25519  | `secret`   | yes |
| `id_rsa`                   | RSA 3072 | none       | yes |
| `id_ed25519_unauthorized`  | ed25519  | none       | **no** (for negative tests) |

Auth methods offered everywhere: `publickey`, `password`, `keyboard-interactive`
(`UsePAM yes`, so keyboard-interactive is a single PAM `Password:` prompt).
`MaxAuthTries 10`.

### target (2201)
- `AllowTcpForwarding yes`, `GatewayPorts clientspecified`, `AllowStreamLocalForwarding yes`,
  `AllowAgentForwarding yes`, PTY allowed. Login shell `/bin/bash`; SFTP via `internal-sftp`.
- Sample tree in `/home/patchbay` (created by `setup-fixtures.sh`):

  ```
  big.bin                    20 MiB, deterministic (AES-128-CTR keystream, zero key/iv)
  big.bin.sha256             4ef0e6ddb3d6dd51ea71bab90f6b2e86fafb1dd4477fdd442a3c095dd1a8516f
  docs/readme.txt            "Hello from the Patchbay test target.\n"
  docs/link-to-readme.txt -> readme.txt
  docs/nested/one.txt
  docs/nested/deeper/deep.txt
  dir with spaces/inner file.txt
  file with spaces.txt
  ünïcödé — 日本語 ✓.txt
  link-to-docs -> docs       (symlink to a directory)
  dangling-link -> does-not-exist
  empty-dir/
  upload/                    (empty, writable)
  readonly.txt               (mode 0444)
  script.sh                  (mode 0755)
  .hidden-file
  ```

### bastion (2202)
- `AllowTcpForwarding yes`. Same user/password/keys. Has the same small sample tree as
  `target` minus `big.bin`.
- `target` resolves to the target container from inside the bastion, so
  `ProxyJump patchbay@127.0.0.1:2202` -> `patchbay@target:22` works.

### sftponly (2203)
- `Match User patchbay`: `ChrootDirectory /srv/sftp`, `ForceCommand internal-sftp -d /`.
- Chroot contents: `/upload/` (owned by patchbay, writable), `/pub/readme.txt` (read-only).
- Exec requests do not run anything: they print `This service allows sftp connections only.`
  and exit with status 1. PTY requests and TCP forwarding are refused.

## Host keys / known_hosts

Host keys are fixed (copied from `hostkeys/<service>/` at build time), so fingerprints are
stable across rebuilds. `known_hosts` in this directory contains entries for
`[127.0.0.1]:220x`, `[localhost]:220x` and the in-network names `target`/`bastion`/`sftponly`.

| Service  | ED25519 fingerprint |
|----------|---------------------|
| target   | `SHA256:IcSyK+So+qrx9a5MKn0Y9C7j5aRY1wGk6FSo+Omxzkc` |
| bastion  | `SHA256:vrCHxitwGYExistaIQzY55rT8YLKk0WPvixgstIA7kk` |
| sftponly | `SHA256:btEUh682Zx+i3kYDHQlzyRHO23Z/ieAnNzWRPtFqQFI` |

(ECDSA and RSA host keys are also offered; see `ssh-keygen -lf hostkeys/*/*.pub`.)

## Manual use with OpenSSH

Git does not preserve `0600` on the private keys and OpenSSH refuses group/world-readable
keys, so first run:
`find docker/test/keys -type f ! -name '*.pub' -exec chmod 600 {} +`

```sh
K=docker/test/keys
O="-o UserKnownHostsFile=docker/test/known_hosts -o IdentitiesOnly=yes"
ssh  $O -i $K/id_ed25519 -p 2201 patchbay@127.0.0.1                         # key
ssh  $O -i $K/id_ed25519_pass -p 2201 patchbay@127.0.0.1                    # passphrase: secret
sshpass -p patchbay ssh $O -o PubkeyAuthentication=no -p 2201 patchbay@127.0.0.1  # password
sftp $O -i $K/id_ed25519 -P 2203 patchbay@127.0.0.1                         # sftp-only chroot
```

ProxyJump (`-J` does not pass `-o` options to the jump hop, so use a config file):

```
Host patchbay-bastion
  HostName 127.0.0.1
  Port 2202
Host patchbay-bastion target
  User patchbay
  IdentityFile docker/test/keys/id_ed25519
  IdentitiesOnly yes
  UserKnownHostsFile docker/test/known_hosts
Host target
  ProxyJump patchbay-bastion
```
