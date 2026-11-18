#!/usr/bin/env bash
# Creates a self-signed code-signing identity for local builds, so Keychain "Always Allow" and
# privacy (TCC) grants are tied to a stable certificate instead of each ad-hoc build's hash.
# Run once per Mac. macOS asks for your login password to trust the certificate.
set -euo pipefail

NAME="Sotto Dev Signing"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -v -p codesigning | grep -q "\"$NAME\""; then
  echo "Identity \"$NAME\" already exists and is valid for code signing."
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "${work:?}"' EXIT

cat > "$work/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF

/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout "$work/key.pem" -out "$work/cert.pem" -config "$work/cert.cnf" 2>/dev/null

pass="$(/usr/bin/openssl rand -hex 16)"
/usr/bin/openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" \
  -name "$NAME" -out "$work/identity.p12" -passout "pass:$pass" 2>/dev/null

security import "$work/identity.p12" -k "$KEYCHAIN" -P "$pass" -T /usr/bin/codesign
security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$work/cert.pem"

security find-identity -v -p codesigning | grep "\"$NAME\"" \
  && echo "Done. Dev builds will be signed with \"$NAME\"."
