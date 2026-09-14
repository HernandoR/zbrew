#!/usr/bin/env bash
set -euo pipefail

# Colors (defined early for error_exit)
MUTED=$'\033[0;2m'
RED=$'\033[0;31m'
ORANGE=$'\033[38;5;214m'
GREEN=$'\033[0;32m'
NC=$'\033[0m'

# Error handler (defined early so it can be used during initialization)
error_exit() {
    local format="$1"
    shift || true

    local exit_code=1
    local last_arg=""
    local arg=""
    for arg in "$@"; do
        last_arg="$arg"
    done
    case "$last_arg" in
    '' | *[!0-9]*)
        ;;
    *)
        exit_code="$last_arg"
        set -- "${@:1:$(($# - 1))}"
        ;;
    esac

    if [[ $# -gt 0 ]]; then
        local msg
        printf -v msg "$format" "$@"
        printf "\r\033[K"
        printf '\033[?25h'
        printf "%b[✗]%b %b\n" "$RED" "$NC" "$msg" >&2
        exit "$exit_code"
    fi

    printf "\r\033[K"
    printf '\033[?25h'
    printf "%b[✗]%b %b\n" "$RED" "$NC" "$format" >&2
    exit "$exit_code"
}

# Cleanup on exit
BUILD_OUTPUT=""
DOWNLOAD_TEMP_DIR=""
DOWNLOADED_ZB_PATH=""
DOWNLOADED_ZBX_PATH=""
cleanup() {
    printf '\033[?25h'  # Restore cursor
    [[ -n "$BUILD_OUTPUT" && -f "$BUILD_OUTPUT" ]] && rm -f "$BUILD_OUTPUT"
    [[ -n "$DOWNLOAD_TEMP_DIR" && -d "$DOWNLOAD_TEMP_DIR" ]] && rm -rf "$DOWNLOAD_TEMP_DIR"
}
trap cleanup EXIT

ZBREW_REPO="https://github.com/HernandoR/zbrew.git"
: "${ZBREW_DIR:=$HOME/.zbrew}"
: "${ZBREW_BIN:=$HOME/.local/bin}"
ORIGINAL_PATH="$PATH"
PREVIOUS_ZB_VERSION=""

# Mirrors get_root_path in zb_cli. A directory that merely happens to exist
# under /opt does not select the root: on Linux that let a stray /opt/zbrew
# override the XDG location that `zb` would pick on its own, so the two
# disagreed about where the install lived.
if [[ "$(uname -s)" == "Darwin" ]]; then
    ZBREW_ROOT="/opt/zbrew"
else
    XDG_DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
    ZBREW_ROOT="$XDG_DATA_HOME/zbrew"
fi

if [[ "$(uname -s)" == "Darwin" ]]; then
    : "${ZBREW_PREFIX:=$ZBREW_ROOT}"
else
    : "${ZBREW_PREFIX:=$ZBREW_ROOT/prefix}"
fi

export ZBREW_ROOT
export ZBREW_PREFIX

# Ensure system tools are used instead of zbrew-installed ones.
# A prior `zb init` adds $ZBREW_PREFIX/bin to PATH, which can cause
# zbrew's curl/git (linked against zbrew's OpenSSL) to be used by
# this script. On some macOS versions that leads to dyld symbol errors.
# see https://github.com/lucasgelfond/zerobrew/issues/288
sanitized_path=""
IFS=':' read -ra _path_parts <<< "$PATH"
for _p in "${_path_parts[@]}"; do
    case "$_p" in
        "$ZBREW_PREFIX"/bin|"$ZBREW_ROOT"/bin) ;;
        *) sanitized_path="${sanitized_path:+$sanitized_path:}$_p" ;;
    esac
done
export PATH="$sanitized_path"
unset sanitized_path _path_parts _p

# Prevent running with sudo - the script handles its own privilege escalation
if [[ $EUID -eq 0 ]]; then
    error_exit "Do not run this script with sudo or as root. The installer will automatically request privileges when needed."
fi

no_modify_path=false
binary_paths=()

usage() {
    printf "zero%bbrew%b Installer\n" "$ORANGE" "$NC"
    printf "\n"
    printf "Usage: install.sh %b[options]%b\n" "$MUTED" "$NC"
    printf "\n"
    printf "Options:\n"
    printf "    -h, --help               %bDisplay this help message%b\n" "$MUTED" "$NC"
    printf "    -b, --binary <path>...   %bInstalls binaries (zb, zbx) to \$ZBREW_BIN%b\n" "$MUTED" "$NC"
    printf "        --no-modify-path     %bDon't modify shell config files (.zshrc, .bashrc, etc.)%b\n" "$MUTED" "$NC"
    printf "\n"
    printf "Examples:%b\n" "$MUTED"
    printf "    ./install.sh --no-modify-path\n"
    printf "    ./install.sh -b /path/to/zb\n"
    printf "    ./install.sh -b /path/to/zb /path/to/zbx%b\n" "$NC"
}

spinner() {
    local msg="$1"
    local pid="$2"
    local spin=$'|/-\\'
    local i=0
    local exit_code=0

    printf '\033[?25l'

    while kill -0 "$pid" 2>/dev/null; do
        i=$(((i + 1) % 4))
        printf "\r%b[%s]%b %b" "$ORANGE" "${spin:$i:1}" "$NC" "$msg"
        sleep 0.1
    done

    wait "$pid" 2>/dev/null && exit_code=0 || exit_code=$?

    printf "\r\033[K"
    printf '\033[?25h'

    return "$exit_code"
}

completed() {
    printf "%b[✓]%b %b\n" "$GREEN" "$NC" "$1"
}

warn() {
    printf "%b[!]%b %b\n" "$ORANGE" "$NC" "$1" >&2
}

zb_version() {
    local zb_path="$1"

    if [[ -x "$zb_path" ]]; then
        "$zb_path" --version 2>/dev/null || true
    fi
}

detect_existing_zb() {
    PREVIOUS_ZB_VERSION="$(zb_version "$ZBREW_BIN/zb")"
}

report_zb_version() {
    local installed_zb="$ZBREW_BIN/zb"
    local installed_version
    installed_version="$(zb_version "$installed_zb")"

    if [[ -z "$installed_version" ]]; then
        return
    fi

    if [[ -z "$PREVIOUS_ZB_VERSION" ]]; then
        completed "Installed ${ORANGE}${installed_version}${NC}"
    elif [[ "$PREVIOUS_ZB_VERSION" == "$installed_version" ]]; then
        completed "${ORANGE}${installed_version}${NC} is already up to date"
    else
        completed "Updated zbrew from ${ORANGE}${PREVIOUS_ZB_VERSION}${NC} to ${ORANGE}${installed_version}${NC}"
    fi

    local current_path_zb
    current_path_zb="$(PATH="$ORIGINAL_PATH" command -v zb 2>/dev/null || true)"
    if [[ -z "$current_path_zb" ]]; then
        warn "zb is installed at ${installed_zb}, but your current shell may not see it until you restart or source your shell config."
    elif [[ "$current_path_zb" != "$installed_zb" ]]; then
        local current_path_version
        current_path_version="$(zb_version "$current_path_zb")"
        warn "Your current shell finds ${current_path_zb}${current_path_version:+ ($current_path_version)} before ${installed_zb}. Restart your terminal or adjust PATH if zb still reports an old version."
    fi
}

check_command() {
    local cmd="$1"
    local install_hint="${2:-}"

    if ! command -v "$cmd" >/dev/null 2>&1; then
        local msg="Required command '$cmd' not found"
        if [[ -n "$install_hint" ]]; then
            msg="$msg. Hint: $install_hint"
        fi
        error_exit "$msg"
    fi
}

install_bin() {
    local target_dir="$1"
    shift
    local paths_to_install=("$@")

    if ! mkdir -p "$target_dir"; then
        error_exit "Failed to create directory: $target_dir"
    fi

    for binary_path in "${paths_to_install[@]}"; do
        if [[ ! -f "$binary_path" ]]; then
            error_exit "Binary not found at ${binary_path}"
        fi

        local binary_name
        binary_name=$(basename "$binary_path")

        if ! install -m755 "$binary_path" "$target_dir/$binary_name"; then
            error_exit "Failed to copy $binary_name to $target_dir"
        fi

        completed "Installed ${ORANGE}$binary_name${NC} to $target_dir"
    done
}

zb_init() {
    local zb_path="$1"
    local no_modify="$2"
    local init_args=()

    if [[ "$no_modify" == "true" ]]; then
        init_args+=("--no-modify-path")
    fi

    "$zb_path" init ${init_args[@]+"${init_args[@]}"} || error_exit "Failed to initialize zbrew"
}

finalize_installation() {
    local no_modify="$1"

    # Verify the binary works
    if ! "$ZBREW_BIN/zb" --version >/dev/null 2>&1; then
        error_exit "Installation succeeded but binary does not execute properly"
    fi

    # Add zb to PATH for current session if not already present
    if [[ ":$PATH:" != *":$ZBREW_BIN:"* ]]; then
        export PATH="$ZBREW_BIN:$PATH"
    fi

    zb_init "$ZBREW_BIN/zb" "$no_modify"
    report_zb_version

    print_logo
    completed "Installation complete"
}

# The glibc Linux release binaries are built on ubuntu-22.04, so they need
# glibc 2.35 or newer. Older distributions and containers (Google Colab, for
# example) must use the statically linked musl build instead.
# see https://github.com/HernandoR/zbrew/issues/10
GLIBC_MIN_MINOR=35

glibc_is_new_enough() {
    local version major minor

    # On musl systems `ldd --version` writes to stderr and exits non-zero,
    # which leaves version empty and selects the musl asset.
    version=$(ldd --version 2>/dev/null | head -n1 | grep -Eo '[0-9]+\.[0-9]+' | tail -n1) || true

    if [[ -z "$version" ]]; then
        return 1
    fi

    major="${version%%.*}"
    minor="${version##*.}"

    if ((major > 2)); then
        return 0
    fi

    ((major == 2 && minor >= GLIBC_MIN_MINOR))
}

linux_asset_candidates() {
    local binary_name="$1"
    local arch="$2"

    if glibc_is_new_enough; then
        printf '%s\n%s\n' "${binary_name}-linux-${arch}" "${binary_name}-linux-${arch}-musl"
    else
        printf '%s\n%s\n' "${binary_name}-linux-${arch}-musl" "${binary_name}-linux-${arch}"
    fi
}

# Prints the release asset names for this platform, most preferred first.
resolve_release_assets() {
    local binary_name="$1"
    local os arch
    os=$(uname -s)
    arch=$(uname -m)

    case "$os/$arch" in
    Darwin/arm64 | Darwin/aarch64)
        echo "${binary_name}-darwin-arm64"
        ;;
    Darwin/x86_64 | Darwin/amd64)
        echo "${binary_name}-darwin-x64"
        ;;
    Linux/arm64 | Linux/aarch64)
        linux_asset_candidates "$binary_name" "arm64"
        ;;
    Linux/x86_64 | Linux/amd64)
        linux_asset_candidates "$binary_name" "x64"
        ;;
    *)
        return 1
        ;;
    esac
}

# A binary can download fine and still be unusable — a glibc build on a host
# with an older glibc exits with a loader error. Check before keeping it.
release_binary_runs() {
    local binary_path="$1"

    [[ -x "$binary_path" ]] || return 1
    "$binary_path" --version >/dev/null 2>&1
}

download_release_binary() {
    local asset_name="$1"
    local output_name="$2"
    local required="${3:-true}"
    local downloaded_path="$DOWNLOAD_TEMP_DIR/${output_name}"
    local download_url="https://github.com/HernandoR/zbrew/releases/latest/download/${asset_name}"

    (
        curl -fsL --retry 3 --retry-delay 1 --connect-timeout 10 \
            "$download_url" \
            -o "$downloaded_path" \
            >/dev/null 2>&1
    ) &
    if ! spinner "Downloading ${ORANGE}${asset_name}${NC} from latest release" $!; then
        if [[ "$required" == "true" ]]; then
            return 1
        fi
        warn "Optional ${ORANGE}${asset_name}${NC} not found."
        return 0
    fi

    if ! chmod +x "$downloaded_path"; then
        if [[ "$required" == "true" ]]; then
            return 1
        fi
        warn "Failed to prepare optional asset ${asset_name}. Continuing without it."
        return 0
    fi

    if [[ "$output_name" == "zb" ]]; then
        DOWNLOADED_ZB_PATH="$downloaded_path"
    elif [[ "$output_name" == "zbx" ]]; then
        DOWNLOADED_ZBX_PATH="$downloaded_path"
    fi

    completed "Downloaded ${ORANGE}${asset_name}${NC} from GitHub Releases"
    return 0
}

try_release_install() {
    local zb_assets_raw asset zb_asset="" zbx_asset
    local zb_candidates=()

    DOWNLOAD_TEMP_DIR=$(mktemp -d)
    DOWNLOADED_ZB_PATH=""
    DOWNLOADED_ZBX_PATH=""

    if ! zb_assets_raw=$(resolve_release_assets "zb"); then
        warn "No prebuilt release binary for zb on $(uname -s)/$(uname -m). Falling back to source build."
        return 1
    fi

    while IFS= read -r asset; do
        if [[ -n "$asset" ]]; then
            zb_candidates+=("$asset")
        fi
    done <<<"$zb_assets_raw"

    if [[ ${#zb_candidates[@]} -eq 0 ]]; then
        warn "No prebuilt release binary for zb on $(uname -s)/$(uname -m). Falling back to source build."
        return 1
    fi

    for asset in "${zb_candidates[@]}"; do
        if download_release_binary "$asset" "zb" "true" && release_binary_runs "$DOWNLOADED_ZB_PATH"; then
            zb_asset="$asset"
            break
        fi
        warn "${ORANGE}${asset}${NC} is unavailable or does not run on this system."
        DOWNLOADED_ZB_PATH=""
        rm -f "$DOWNLOAD_TEMP_DIR/zb"
    done

    if [[ -z "$zb_asset" ]]; then
        warn "No usable release binary for zb on $(uname -s)/$(uname -m). Falling back to source build."
        return 1
    fi

    # Take zbx from the same build variant as the zb that works here.
    zbx_asset="zbx-${zb_asset#zb-}"
    download_release_binary "$zbx_asset" "zbx" "false"

    local binaries_to_install=("$DOWNLOADED_ZB_PATH")
    if [[ -n "$DOWNLOADED_ZBX_PATH" && -f "$DOWNLOADED_ZBX_PATH" ]]; then
        binaries_to_install+=("$DOWNLOADED_ZBX_PATH")
    fi

    install_bin "$ZBREW_BIN" "${binaries_to_install[@]}"
    finalize_installation "$no_modify_path"
    return 0
}

print_logo() {
    printf "\n"
    printf "%b▄▄▄▄▄ ▄▄▄▄▄ ▄▄▄▄   ▄▄▄ %b ▄▄▄▄  ▄▄▄▄  ▄▄▄▄▄ ▄▄   ▄▄\n" "$NC" "$ORANGE"
    printf "%b  ▄█▀ ██▄▄  ██▄█▄ ██▀██%b ██▄██ ██▄█▄ ██▄▄  ██ ▄ ██\n" "$NC" "$ORANGE"
    printf "%b▄██▄▄ ██▄▄▄ ██ ██ ▀███▀%b ██▄█▀ ██ ██ ██▄▄▄  ▀█▀█▀ \n" "$NC" "$ORANGE"
    printf "\n"

    printf "%bStart installing %bPackages%b with %bzbrew%b:\n\n" "$MUTED" "$NC" "$MUTED" "$ORANGE" "$NC"
    printf "  zb install %bffmpeg%b    # Install a Package%b\n" "$ORANGE" "$MUTED" "$NC"
    printf "  zbx %byetris%b           # Single-time Run%b\n\n" "$ORANGE" "$MUTED" "$NC"
    printf "%bFor more information visit %bhttps://github.com/HernandoR/zbrew\n\n" "$MUTED" "$NC"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
    -h | --help)
        usage
        exit 0
        ;;
    --no-modify-path)
        no_modify_path=true
        shift
        ;;
    -b | --binary)
        if [[ -n "${2:-}" ]]; then
            binary_paths+=("$2")
            shift 2
            if [[ -n "${1:-}" && "${1:0:1}" != "-" ]]; then
                binary_paths+=("$1")
                shift
            fi
        else
            error_exit "--binary requires a path argument"
        fi
        ;;
    *)
        error_exit "Unknown option '%s'" "$1"
        ;;
    esac
done

detect_existing_zb

# Skip all if binary path is provided
if [[ ${#binary_paths[@]} -gt 0 ]]; then
    install_bin "$ZBREW_BIN" "${binary_paths[@]}"
    finalize_installation "$no_modify_path"
    exit 0
fi

# Check for required commands
check_command "curl" "Install curl using your package manager (e.g., 'brew install curl' on macOS)"
check_command "git" "Install git using your package manager (e.g., 'brew install git' on macOS)"
check_command "mkdir" "Your system should have mkdir installed by default"
check_command "cp" "Your system should have cp installed by default"
check_command "chmod" "Your system should have chmod installed by default"
check_command "uname" "Your system should have uname installed by default"

# Try latest prebuilt release first, then fall back to source build if needed.
if try_release_install; then
    exit 0
fi

# Check for Rust/Cargo
if ! command -v cargo >/dev/null 2>&1; then
    (
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    ) &
    if ! spinner "Installing ${ORANGE}Rust toolchain${NC}" $!; then
        error_exit "Failed to install Rust toolchain. Check your network connection and try again."
    fi
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
    completed "${ORANGE}Rust toolchain${NC} installed"
fi

# Ensure cargo is available
if ! command -v cargo >/dev/null 2>&1; then
    error_exit "Cargo not found after installing Rust. Try restarting your terminal or running: source ~/.cargo/env"
fi

# Clone or update repo
if [[ -d "$ZBREW_DIR" ]]; then
    (
        cd "$ZBREW_DIR" || exit 1
        if ! git fetch --depth=1 origin main >/dev/null 2>&1; then
            printf "Failed to fetch updates\n" >&2
            exit 1
        fi
        if ! git reset --hard origin/main >/dev/null 2>&1; then
            printf "Failed to reset to origin/main\n" >&2
            exit 1
        fi
    ) &
    if ! spinner "Updating ${ORANGE}zbrew${NC} repository" $!; then
        error_exit "Failed to update zbrew repository. Check your network connection and permissions."
    fi
    completed "Updated ${ORANGE}zbrew${NC} repository"
    cd "$ZBREW_DIR" || error_exit "Failed to enter directory: $ZBREW_DIR"
else
    (
        if ! git clone --depth 1 "$ZBREW_REPO" "$ZBREW_DIR" >/dev/null 2>&1; then
            printf "Failed to clone repository\n" >&2
            exit 1
        fi
    ) &
    if ! spinner "Cloning ${ORANGE}zbrew${NC} repository" $!; then
        error_exit "Failed to clone zbrew repository. Check your network connection and that the repository exists."
    fi
    completed "Cloned ${ORANGE}zbrew${NC} repository"
    cd "$ZBREW_DIR" || error_exit "Failed to enter directory: $ZBREW_DIR"
fi

# Build
if [[ -d "$ZBREW_PREFIX/lib/pkgconfig" ]]; then
    export PKG_CONFIG_PATH="$ZBREW_PREFIX/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
fi
if [[ -d "/opt/homebrew/lib/pkgconfig" ]] && [[ ! "${PKG_CONFIG_PATH:-}" =~ "/opt/homebrew/lib/pkgconfig" ]]; then
    export PKG_CONFIG_PATH="/opt/homebrew/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
fi

# Use a temp file to capture cargo's JSON output for binary path detection
BUILD_OUTPUT=$(mktemp)

(
    if ! cargo build --release --bin zb --bin zbx --message-format=json > "$BUILD_OUTPUT" 2>&1; then
        exit 1
    fi
) &
if ! spinner "Building ${ORANGE}zbrew${NC}" $!; then
    error_exit "Failed to build zbrew. Run 'cargo build --release --bin zb --bin zbx' to see details."
fi
completed "Built ${ORANGE}zbrew${NC}"

# Parse cargo's JSON output to find the actual binary paths
# This handles custom CARGO_TARGET_DIR, .cargo/config.toml target-dir, etc.
parse_binary_path() {
    local binary_name="$1"
    local path
    # Each JSON line from cargo is self-contained. Find lines that:
    # 1. Are compiler-artifact messages (contain "reason":"compiler-artifact")
    # 2. Have an executable (contain "executable":)
    # 3. Match our binary name (contain "name":"$binary_name")
    # The name field in target uniquely identifies the binary
    path=$(grep "\"reason\":\"compiler-artifact\"" "$BUILD_OUTPUT" \
        | grep "\"executable\":" \
        | grep "\"name\":\"$binary_name\"" \
        | sed -E 's/.*"executable":"([^"]+)".*/\1/' \
        | tail -n1)
    echo "$path"
}

ZB_PATH=$(parse_binary_path "zb")
ZBX_PATH=$(parse_binary_path "zbx")

if [[ -z "$ZB_PATH" || ! -f "$ZB_PATH" ]]; then
    error_exit "Build succeeded but could not locate zb binary. Check cargo configuration."
fi

if [[ -z "$ZBX_PATH" || ! -f "$ZBX_PATH" ]]; then
    error_exit "Build succeeded but could not locate zbx binary. Check cargo configuration."
fi

install_bin "$ZBREW_BIN" "$ZB_PATH" "$ZBX_PATH"
finalize_installation "$no_modify_path"
