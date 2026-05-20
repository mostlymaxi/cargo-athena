set -e
case "$(uname -m)" in
@@ARMS@@  *) echo "athena: unsupported arch $(uname -m)" >&2; exit 1 ;;
esac
chmod +x @@BIN_DIR@@/app-$__t
export CARGO_ATHENA_OUTPUT=/athena/result
exec @@BIN_DIR@@/app-$__t --cargo-athena-template @@TEMPLATE@@
