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
TT_BINARY="$PROJECT_DIR/target/release/tt"
SERVE_BINARY="$PROJECT_DIR/target/release/tt-serve"

# Build if needed
if [ ! -f "$TT_BINARY" ] || [ ! -f "$SERVE_BINARY" ]; then
    echo "Building tt binaries..."
    (cd "$PROJECT_DIR" && cargo build --release --bin tt --bin tt-serve)
fi

echo "Deploying tt and tt-serve to $REMOTE..."

# Create ~/.dotfiles/bin on remote if it doesn't exist
ssh "$REMOTE" 'mkdir -p ~/.dotfiles/bin'

scp "$TT_BINARY" "$REMOTE:~/.dotfiles/bin/tt"
scp "$SERVE_BINARY" "$REMOTE:~/.dotfiles/bin/tt-serve"

# Make executables and link the daemon where its systemd unit expects it
ssh "$REMOTE" 'chmod +x ~/.dotfiles/bin/tt ~/.dotfiles/bin/tt-serve && mkdir -p ~/.local/bin && ln -sfn ~/.dotfiles/bin/tt-serve ~/.local/bin/tt-serve'

# Add to PATH if not already there
ssh "$REMOTE" 'grep -q "PATH=.*\.dotfiles/bin" ~/.bashrc 2>/dev/null || echo "export PATH=\"\$HOME/.dotfiles/bin:\$PATH\"" >> ~/.bashrc'

echo "Binaries deployed to ~/.dotfiles/bin/tt and ~/.dotfiles/bin/tt-serve"

# Verify deployment
echo ""
echo "Verifying installation..."
ssh "$REMOTE" '~/.dotfiles/bin/tt --version'
ssh "$REMOTE" '~/.dotfiles/bin/tt-serve --version'

# Configure tmux hook if requested
if [ "$CONFIGURE_TMUX" = true ]; then
    echo ""
    echo "Configuring tmux hook..."

    # Install the project's hook config and source it, rather than echoing a hook
    # into ~/.tmux.conf. The inline hook this used to append was wrong four ways and
    # every one of them failed silently:
    #   - `pane-focus-in` is not a hook name tmux 3.7b knows, so it never fired once;
    #     `set-hook` does not error on an unknown name.
    #   - `run-shell` without -b blocks the pane switch on the tt invocation.
    #   - values were interpolated raw, so a pane path or session name containing
    #     $(...) or backticks would execute as shell code.
    #   - it installed 1 of the 5 hooks and none of the scroll capture, and
    #     `tmux_scroll` feeds direct time.
    HOOK_SRC="$PROJECT_DIR/config/tmux-hook.conf"
    REMOTE_HOOK='$HOME/.config/tt-tmux-hook.conf'
    ssh "$REMOTE" 'mkdir -p ~/.config'
    scp "$HOOK_SRC" "$REMOTE:$REMOTE_HOOK"

    SOURCE_LINE="if-shell '[ -f $REMOTE_HOOK ]' 'source-file $REMOTE_HOOK'"
    if ssh "$REMOTE" "grep -qF 'tt-tmux-hook.conf' ~/.tmux.conf 2>/dev/null"; then
        echo "tmux hook already sourced from ~/.tmux.conf (config refreshed)"
    else
        # Any pre-existing inline tt hooks are left in place rather than edited out:
        # this script must not rewrite a hand-maintained dotfile. It reports them so
        # the operator can remove them -- leaving both means every focus event is
        # written twice.
        if ssh "$REMOTE" "grep -q 'tt ingest' ~/.tmux.conf 2>/dev/null"; then
            echo "WARNING: ~/.tmux.conf already contains inline 'tt ingest' hooks."
            echo "         Remove them, or every focus event will be recorded twice."
        fi
        ssh "$REMOTE" "printf '\n# Time tracker: pane focus + scroll capture\n%s\n' \"$SOURCE_LINE\" >> ~/.tmux.conf"
        echo "Added a source-file line for the hook config to ~/.tmux.conf"
    fi
    echo ""
    echo "Reload tmux config with: tmux source-file ~/.tmux.conf"
    echo "Then verify by the events it produces, not by the absence of an error:"
    echo "  ssh $REMOTE 'tmux show-hooks -g | grep -c \"tt ingest\"'   # expect 5"
fi

echo ""
echo "Deployment complete!"
echo ""
echo "Next steps:"
echo "1. Add tt to PATH on remote (or source ~/.bashrc)"
echo "2. Add tmux hook (if not done): run this script with --configure-tmux"
echo "3. Enable the daemon:"
echo "   ssh $REMOTE 'mkdir -p ~/.config/systemd/user'"
echo "   scp config/tt-serve.service $REMOTE:~/.config/systemd/user/"
echo "   ssh $REMOTE 'systemctl --user daemon-reload && systemctl --user enable --now tt-serve'"
echo "   NOTE: that unit wraps ExecStart in 'secrets ANTHROPIC_API_KEY --'. On a remote"
echo "   without secretsd, edit it to supply the key another way (the unit names one),"
echo "   or the daemon starts healthy and classifies nothing. Check with:"
echo "     ssh $REMOTE 'tt status | grep classifier'"
echo "4. Start syncing: tt sync $REMOTE"
