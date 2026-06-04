#!/usr/bin/env sh
# Throwaway MinIO for testing s3.* (presign + usage) against a real S3 store.
#
#   ./minio.sh start    # run MinIO (--rm) + seed user-a/ and user-b/
#   ./minio.sh stop     # stop it (auto-removed) + tidy the test network
#   ./minio.sh seed     # re-seed the bucket without restarting
#   ./minio.sh status   # is it running?
#
# The container uses `docker run --rm`, so stopping it deletes it — no leftover
# state between test runs. MinIO + the jsbox container share a dedicated network
# (`jsbox-test-net`) so jsbox reaches the store by name at http://jsbox-minio:9000.
set -eu

# Stop Git-Bash/MSYS from rewriting container-side paths (e.g. /data) into
# Windows paths when calling the native docker.exe. No-op on Linux/macOS/WSL.
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

NAME="jsbox-minio"           # MinIO container name (also its hostname on the net)
NETWORK="jsbox-test-net"     # shared with the running jsbox container
USER="minioadmin"
PASS="minioadmin"
BUCKET="uploads"
IMAGE="minio/minio:latest"
MC_IMAGE="minio/mc:latest"
PORT_API=9000
PORT_CONSOLE=9001

running() { docker ps --format '{{.Names}}' | grep -qx "$1"; }

start() {
  docker network inspect "$NETWORK" >/dev/null 2>&1 || docker network create "$NETWORK" >/dev/null
  if running "$NAME"; then
    echo "MinIO '$NAME' already running."
  else
    echo "Starting MinIO ($NAME) ..."
    docker run -d --rm --name "$NAME" --network "$NETWORK" \
      -p "${PORT_API}:9000" -p "${PORT_CONSOLE}:9001" \
      -e "MINIO_ROOT_USER=$USER" -e "MINIO_ROOT_PASSWORD=$PASS" \
      "$IMAGE" server /data --console-address ":9001" >/dev/null
  fi
  seed
  connect_jsbox
  info
}

# Seeds two "folders" (key prefixes) with known sizes so s3.usage has something
# exact to total:  user-a/ = 300 bytes / 2 objects,  user-b/ = 50 bytes / 1 object.
seed() {
  echo "Seeding bucket '$BUCKET' (user-a/, user-b/) ..."
  docker run --rm --network "$NETWORK" --entrypoint sh \
    -e "MC_HOST_t=http://${USER}:${PASS}@${NAME}:9000" \
    -e "BKT=${BUCKET}" \
    "$MC_IMAGE" -c '
set -e
# wait until MinIO accepts connections
i=0
until mc ls t >/dev/null 2>&1; do
  i=$((i + 1)); [ "$i" -gt 30 ] && { echo "MinIO did not become ready" >&2; exit 1; }
  sleep 1
done
mc mb -p "t/$BKT" 2>/dev/null || true
printf "%*s" 100 "" | mc pipe "t/$BKT/user-a/one.txt"
printf "%*s" 200 "" | mc pipe "t/$BKT/user-a/two.txt"
printf "%*s" 50  "" | mc pipe "t/$BKT/user-b/solo.txt"
echo "--- objects in $BKT ---"
mc ls -r "t/$BKT"
'
}

# If the jsbox container is up, attach it to the test network so its
# config.s3.endpoint of http://jsbox-minio:9000 resolves.
connect_jsbox() {
  running jsbox || return 0
  if docker network inspect "$NETWORK" --format '{{range .Containers}}{{.Name}} {{end}}' \
       | grep -qw jsbox; then
    return 0
  fi
  docker network connect "$NETWORK" jsbox && echo "Attached running 'jsbox' container to $NETWORK."
}

stop() {
  running jsbox && docker network disconnect "$NETWORK" jsbox 2>/dev/null || true
  if running "$NAME"; then
    echo "Stopping MinIO ($NAME) ... (auto-removed via --rm)"
    docker stop "$NAME" >/dev/null
  else
    echo "MinIO '$NAME' is not running."
  fi
  docker network rm "$NETWORK" >/dev/null 2>&1 || true
}

info() {
  cat <<EOF

MinIO is up (throwaway — deleted on stop):
  S3 API (host)     : http://localhost:${PORT_API}     (path-style)
  Console (browser) : http://localhost:${PORT_CONSOLE}     user: ${USER}  pass: ${PASS}
  Bucket            : ${BUCKET}
  Seeded            : user-a/ = 300 bytes / 2 objects
                      user-b/ =  50 bytes / 1 object

From the jsbox container, point config.s3 at the store by network name:
  "endpoint": "http://${NAME}:${PORT_API}", "path_style": true, "region": "us-east-1",
  "bucket": "${BUCKET}", "access_key": "${USER}", "secret_key": "${PASS}"
(config.json needs "debug": true so the internal host passes the SSRF guard.)

Test it:  ./run.sh usage
EOF
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  seed) seed ;;
  status) running "$NAME" && echo "running" || echo "stopped" ;;
  *) echo "usage: $0 {start|stop|seed|status}" >&2; exit 1 ;;
esac
