#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

certs=$(mktemp -d "${TMPDIR:-/tmp}/solder-tls.XXXXXX")
prefix="solder-tls-$$-$RANDOM"
containers=()

cleanup() {
  for container in "${containers[@]}"; do
    docker rm -fv "$container" >/dev/null 2>&1 || true
  done
  rm -rf "$certs"
}
trap cleanup EXIT

openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
  -subj '/CN=Solder test CA' -keyout "$certs/ca.key" -out "$certs/ca.crt" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -subj '/CN=localhost' \
  -keyout "$certs/server.key" -out "$certs/server.csr" >/dev/null 2>&1
printf 'subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\n' > "$certs/extensions.cnf"
openssl x509 -req -days 1 -in "$certs/server.csr" -CA "$certs/ca.crt" \
  -CAkey "$certs/ca.key" -CAcreateserial -extfile "$certs/extensions.cnf" \
  -out "$certs/server.crt" >/dev/null 2>&1
cat "$certs/server.crt" "$certs/server.key" > "$certs/server.pem"
chmod 755 "$certs"
chmod 644 "$certs/server.key" "$certs/server.pem"

start() {
  local engine=$1 port=$2
  shift 2
  containers+=("$prefix-$engine")
  docker run -d --name "$prefix-$engine" --mount "type=bind,src=$certs,dst=/certs,readonly" \
    -p "127.0.0.1::$port" "$@" >/dev/null
}

start pg 5432 -e POSTGRES_PASSWORD=solder -e POSTGRES_DB=solder postgres:17-alpine \
  sh -c 'mkdir -p /tls && cp /certs/server.crt /certs/server.key /tls/ && chown -R postgres /tls && chmod 600 /tls/server.key && exec docker-entrypoint.sh postgres -c ssl=on -c ssl_cert_file=/tls/server.crt -c ssl_key_file=/tls/server.key'
start mysql 3306 -e MYSQL_ROOT_PASSWORD=solder -e MYSQL_DATABASE=solder mysql:8.4 \
  --ssl-ca=/certs/ca.crt --ssl-cert=/certs/server.crt --ssl-key=/certs/server.key
start redis 6380 redis:7-alpine redis-server --port 0 --tls-port 6380 \
  --tls-cert-file /certs/server.crt --tls-key-file /certs/server.key \
  --tls-ca-cert-file /certs/ca.crt --tls-auth-clients no
start mongo 27017 mongo:8 mongod --bind_ip_all --tlsMode requireTLS \
  --tlsCertificateKeyFile /certs/server.pem --tlsCAFile /certs/ca.crt \
  --tlsAllowConnectionsWithoutCertificates

ready() {
  local container=$1
  shift
  for attempt in {1..90}; do
    if docker exec "$container" "$@" >/dev/null 2>&1; then
      return
    fi
    if [[ $(docker inspect -f '{{.State.Running}}' "$container") != true ]]; then
      break
    fi
    sleep 1
  done
  docker logs --tail 30 "$container" >&2
  printf 'TLS test server did not become ready: %s\n' "$container" >&2
  return 1
}

ready "$prefix-pg" pg_isready -h 127.0.0.1 -U postgres
ready "$prefix-mysql" mysql -h127.0.0.1 --ssl-mode=REQUIRED -uroot -psolder -e 'SELECT 1'
ready "$prefix-redis" redis-cli --tls --cacert /certs/ca.crt -h localhost -p 6380 ping
ready "$prefix-mongo" mongosh --quiet --tls --tlsCAFile /certs/ca.crt --host localhost \
  --eval 'db.runCommand({ping: 1})'

port() {
  docker port "$prefix-$1" "$2/tcp" | sed 's/.*://'
}

export SOLDER_TEST_TLS_CA="$certs/ca.crt"
export SOLDER_TEST_POSTGRES_TLS="postgres://postgres:solder@localhost:$(port pg 5432)/solder"
export SOLDER_TEST_MYSQL_TLS="mysql://root:solder@localhost:$(port mysql 3306)/solder"
export SOLDER_TEST_REDIS_TLS="rediss://localhost:$(port redis 6380)"
export SOLDER_TEST_MONGO_TLS="mongodb://localhost:$(port mongo 27017)/solder"
export SOLDER_TEST_MYSQL="${SOLDER_TEST_MYSQL:-$SOLDER_TEST_MYSQL_TLS}"
cargo test --profile ci --locked -p db --test servers tls -- --nocapture

probe() {
  SOLDER_PROBE_URL="$1" cargo run --quiet --profile ci --locked -p db --example probe -- --require-tls
}

probe "$SOLDER_TEST_POSTGRES_TLS?sslmode=verify-full&sslrootcert=$SOLDER_TEST_TLS_CA"
probe "$SOLDER_TEST_MYSQL_TLS?ssl-ca=$SOLDER_TEST_TLS_CA"
probe "$SOLDER_TEST_REDIS_TLS/?sslrootcert=$SOLDER_TEST_TLS_CA"
probe "$SOLDER_TEST_MONGO_TLS?tls=true&tlsCAFile=$SOLDER_TEST_TLS_CA"
if probe "$SOLDER_TEST_POSTGRES_TLS?sslmode=disable"; then
  printf 'Probe accepted explicitly disabled TLS\n' >&2
  exit 1
fi
