#!/bin/sh
set -e

# Cumments container entrypoint.
#
# The image starts as root only long enough to make the data directory
# writable by the process user, then drops privileges before running the
# service. This mirrors the pattern used by official images such as postgres.
#
# Bind-mounted directories are initially owned by the Docker daemon (root),
# so without this step containers fail with "Permission denied" when trying
# to create the SQLite database. Set PUID/PGID to your host user's ids
# (`id -u` / `id -g`) to make the data files owned by that user instead of
# the bundled `cumments` user.

DEFAULT_UID="$(id -u cumments)"
DEFAULT_GID="$(id -g cumments)"
PUID="${PUID:-$DEFAULT_UID}"
PGID="${PGID:-$DEFAULT_GID}"

mkdir -p /srv/cumments
chown -R "${PUID}:${PGID}" /srv/cumments

exec su-exec "${PUID}:${PGID}" "$@"
