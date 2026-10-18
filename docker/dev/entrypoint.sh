#!/bin/sh
# Make sure the (host) uid/gid we run as has passwd/group entries, then exec the command.
uid=$(id -u); gid=$(id -g)
if ! getent group "$gid" >/dev/null 2>&1; then
    echo "dev:x:$gid:" >> /etc/group
fi
if ! getent passwd "$uid" >/dev/null 2>&1; then
    echo "dev:x:$uid:$gid:Patchbay dev:${HOME:-/cache/home}:/bin/bash" >> /etc/passwd
fi
mkdir -p "${HOME:-/cache/home}" 2>/dev/null || true
exec "$@"
