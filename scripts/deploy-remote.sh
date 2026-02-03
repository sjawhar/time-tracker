#!/bin/bash
# Deploy tt binary to a remote dev server
# Usage: ./scripts/deploy-remote.sh user@remote

set -euo pipefail

if [ $# -lt 1 ]; then
    echo "Usage: $0 <remote> [--configure-tmux]"
    echo ""
    echo "Examples:"
    echo "  $0 user@devserver.example.com"
    echo "  $0 mydevbox --configure-tmux"
    echo ""
    echo "Options:"
    echo "  --configure-tmux  Also add the tmux hook to ~/.tmux.conf on remote"
    exit 1
fi

REMOTE="$1"
CONFIGURE_TMUX=false

if [ "${2:-}" = "--configure-tmux" ]; then
    CONFIGURE_TMUX=true
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BINARY="$PROJECT_DIR/target/release/tt"

# Build if needed
if [ ! -f "$BINARY" ]; then
    echo "Building tt binary..."
    (cd "$PROJECT_DIR" && cargo build --release)
fi

echo "Deploying tt to $REMOTE..."

# Create ~/.local/bin on remote if it doesn't exist
ssh "$REMOTE" 'mkdir -p ~/.local/bin'

# Copy binary
scp "$BINARY" "$REMOTE:~/.local/bin/tt"

# Make executable
ssh "$REMOTE" 'chmod +x ~/.local/bin/tt'

# Add to PATH if not already there
ssh "$REMOTE" 'grep -q "PATH=.*\.local/bin" ~/.bashrc 2>/dev/null || echo "export PATH=\"\$HOME/.local/bin:\$PATH\"" >> ~/.bashrc'

echo "Binary deployed to ~/.local/bin/tt"

# Verify deployment
echo ""
echo "Verifying installation..."
ssh "$REMOTE" '~/.local/bin/tt --version'

# Configure tmux hook if requested
if [ "$CONFIGURE_TMUX" = true ]; then
    echo ""
    echo "Configuring tmux hook..."

    TMUX_HOOK='set-hook -g pane-focus-in '\''run-shell "tt ingest pane-focus --pane=#{pane_id} --cwd=#{pane_current_path} --session=#{session_name} --window=#{window_index}"'\'''

    # Check if hook already exists
    if ssh "$REMOTE" "grep -q 'tt ingest pane-focus' ~/.tmux.conf 2>/dev/null"; then
        echo "tmux hook already configured in ~/.tmux.conf"
    else
        # Add hook to tmux.conf
        ssh "$REMOTE" "echo '' >> ~/.tmux.conf && echo '# Time tracker - capture pane focus events' >> ~/.tmux.conf && echo '$TMUX_HOOK' >> ~/.tmux.conf"
        echo "Added tmux hook to ~/.tmux.conf"
        echo ""
        echo "Reload tmux config with: tmux source-file ~/.tmux.conf"
    fi
fi

echo ""
echo "Deployment complete!"
echo ""
echo "Next steps:"
echo "1. Add tt to PATH on remote (or source ~/.bashrc)"
echo "2. Add tmux hook (if not done): run this script with --configure-tmux"
echo "3. Start syncing: tt sync $REMOTE"
