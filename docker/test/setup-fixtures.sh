#!/bin/sh
# Creates deterministic sample files for file-browser / transfer tests.
set -eu
ROLE="$1"

# 20 MiB of incompressible but deterministic data (AES-CTR keystream of zeros), so the
# checksum is stable across rebuilds:  sha256 = see docker/test/README.md / big.bin.sha256
gen_big() {
    head -c 20971520 /dev/zero \
      | openssl enc -aes-128-ctr -nosalt -K 00000000000000000000000000000000 \
            -iv 00000000000000000000000000000000 > "$1"
    sha256sum "$1" | cut -d' ' -f1 > "$1.sha256"
}

case "$ROLE" in
target|bastion)
    H=/home/patchbay
    mkdir -p "$H/docs/nested/deeper" "$H/dir with spaces" "$H/empty-dir" "$H/upload"
    printf 'Hello from the Patchbay test target.\n' > "$H/docs/readme.txt"
    printf 'deep file\n' > "$H/docs/nested/deeper/deep.txt"
    printf 'level one\n' > "$H/docs/nested/one.txt"
    printf 'spaces\n' > "$H/file with spaces.txt"
    printf 'unicode\n' > "$H/ünïcödé — 日本語 ✓.txt"
    printf 'inside a dir with spaces\n' > "$H/dir with spaces/inner file.txt"
    printf 'hidden\n' > "$H/.hidden-file"
    printf 'read only\n' > "$H/readonly.txt" && chmod 444 "$H/readonly.txt"
    printf '#!/bin/sh\necho hello\n' > "$H/script.sh" && chmod 755 "$H/script.sh"
    ln -s docs "$H/link-to-docs"
    ln -s readme.txt "$H/docs/link-to-readme.txt"
    ln -s does-not-exist "$H/dangling-link"
    if [ "$ROLE" = target ]; then gen_big "$H/big.bin"; fi
    chown -R -h patchbay:patchbay "$H"
    ;;
sftponly)
    # ChrootDirectory must be root-owned and not writable by anyone else.
    install -d -m 755 -o root -g root /srv/sftp
    install -d -m 755 -o patchbay -g patchbay /srv/sftp/upload
    install -d -m 755 -o root -g root /srv/sftp/pub
    printf 'You are inside an SFTP-only chroot.\n' > /srv/sftp/pub/readme.txt
    ;;
*)
    echo "unknown role $ROLE" >&2; exit 1 ;;
esac
