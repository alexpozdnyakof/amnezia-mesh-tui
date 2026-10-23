#!/bin/bash
# Test stand: a "classic" AmneziaWG container to exercise the adopt wizard
# (M1). Disposable by design — keys and container are throwaway, nothing
# touches a real server. Requires: local docker (colima), image
# amneziavpn/amneziawg-go (pulled for linux/amd64, runs via Rosetta on arm64).
set -euo pipefail

IMAGE="${AWG_TEST_IMAGE:-amneziavpn/amneziawg-go:latest}"
NAME="${AWG_TEST_CONTAINER:-amnezia-legacy-test}"
HERE="$(cd "$(dirname "$0")" && pwd)"
GEN="$HERE/.gen"
HUB="10.8.1.1"
PORT=51820

awg() { docker run --rm --platform linux/amd64 --network none --entrypoint awg "$IMAGE" "$@"; }
awg_stdin() { docker run --rm -i --platform linux/amd64 --network none --entrypoint awg "$IMAGE" "$@"; }

mkdir -p "$GEN"

server_priv="$(awg genkey)"
server_pub="$(printf '%s' "$server_priv" | awg_stdin pubkey)"

peer_pub() { local priv; priv="$(awg genkey)"; printf '%s' "$priv" | awg_stdin pubkey; }

p1_pub="$(peer_pub)"; p1_psk="$(awg genpsk)"
p2_pub="$(peer_pub)"; p2_psk="$(awg genpsk)"
p3_pub="$(peer_pub)"; p3_psk="$(awg genpsk)"

# Obfuscation profile, same ranges as awg_mesh/keygen.gen_obfuscation.
jc=$((RANDOM % 5 + 3)); jmin=$((RANDOM % 11 + 10)); jmax=$((jmin + RANDOM % 21 + 30))
h() { echo $((RANDOM * 32768 + RANDOM)); }
hpk="$(openssl rand -base64 32)"

cat > "$GEN/awg0.conf" <<EOF
[Interface]
PrivateKey = $server_priv
Address = $HUB/24
ListenPort = $PORT
MTU = 1280
Jc = $jc
Jmin = $jmin
Jmax = $jmax
S1 = $((RANDOM % 190 + 30))
S2 = $((RANDOM % 190 + 30))
S3 = $((RANDOM % 190 + 30))
S4 = $((RANDOM % 190 + 30))
H1 = $(h)
H2 = $(h)
H3 = $(h)
H4 = $(h)
HeaderProtectionKey = $hpk
ContentPaddingAddition = 2-10
RekeyAfterTime = 120-180
RekeyTimeout = 5-10
RejectAfterTime = 540-600
KeepaliveTimeout = 8-12
MaxHandshakeAttempts = 80-100
RandomTrailers = on
DisableCookies = on

[Peer]
# test-phone (added 2026-09-14T08:30:00Z)
PublicKey = $p1_pub
PresharedKey = $p1_psk
AllowedIPs = 10.8.1.2/32
PersistentKeepalive = 25

[Peer]
PublicKey = $p2_pub
PresharedKey = $p2_psk
AllowedIPs = 10.8.1.3/32
PersistentKeepalive = 25

[Peer]
# MacBook Pro (RDP fix)
PublicKey = $p3_pub
PresharedKey = $p3_psk
AllowedIPs = 10.8.1.4/32
PersistentKeepalive = 25
EOF
chmod 600 "$GEN/awg0.conf"

docker rm -f "$NAME" >/dev/null 2>&1 || true
# Local stand has no amnezia kernel module (colima), and awg-quick's kernel
# path rejects Amnezia params. Bring the interface up in userspace instead:
# amneziawg-go daemon + awg setconf with [Peer]-only view of the config.
docker run -d --name "$NAME" --hostname amnezia-awg \
  --cap-add NET_ADMIN --device /dev/net/tun:/dev/net/tun \
  --platform linux/amd64 \
  -p "$PORT:$PORT/udp" \
  -v "$GEN/awg0.conf:/opt/amnezia/awg/awg0.conf:ro" \
  --entrypoint /bin/sh "$IMAGE" \
  -c "amneziawg-go awg0 & sleep 1; awk '/^\[Peer\]/{f=1} f' /opt/amnezia/awg/awg0.conf > /tmp/peers.conf; awg setconf awg0 /tmp/peers.conf; ip address add $HUB/24 dev awg0; ip link set mtu 1280 up dev awg0; tail -f /dev/null" >/dev/null

echo "test stand up: container '$NAME' (image $IMAGE, udp $PORT)"
echo "adopt source config: $GEN/awg0.conf (3 peers: clean / no-comment / sanitize-me)"
