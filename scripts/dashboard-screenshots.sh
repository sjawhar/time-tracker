#!/bin/bash
set -e

cd /home/sami/Code/time-tracker/default

# Build frontend
echo "Building frontend..."
cd crates/tt-server/web
npm run build
cd ../../..

# Build backend
echo "Building backend..."
cargo build --bin seed_and_serve
cargo build --bin tt-serve

PORT=4174
SERVER_PID=""

# Never leave the seeded server running: it holds the port, and because it
# inherits stdout it also keeps any non-interactive caller's pipe open, which
# makes this script appear to hang long after it has logically finished.
stop_server() {
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    SERVER_PID=""
}
trap stop_server EXIT INT TERM

take_screenshot() {
    local state=$1
    local out_name=$2
    local width=$3
    local height=$4

    echo "Seeding DB for state: $state"
    SEED_STATE=$state ./target/debug/seed_and_serve

    echo "Starting server on port $PORT..."
    # Redirect the server's output. Left on the inherited stdout it holds the
    # caller's pipe open for the life of the process.
    TT_TODO_STORE_PATH=/tmp/tt_seed_todos ./target/debug/tt-serve \
        --port "$PORT" --db /tmp/tt_seed.db >/tmp/tt_seed_serve.log 2>&1 &
    SERVER_PID=$!

    # Wait for server to start
    sleep 2

    echo "Taking screenshot..."
    # The URL is required. Without it screenshot.js falls back to its default
    # port and captures whatever else happens to be listening there -- never
    # the server this function just seeded and started.
    (cd crates/tt-server/web \
        && node scripts/screenshot.js \
            "../../../.screenshots/$out_name" "$width" "$height" "http://localhost:$PORT")

    echo "Stopping server..."
    stop_server
}

# 1080x1920 portrait screenshots
take_screenshot "ALIGNED" "aligned.png" 1080 1920
take_screenshot "DRIFTING" "drifting.png" 1080 1920
take_screenshot "UNKNOWN" "unknown.png" 1080 1920
take_screenshot "CLASSIFIER_FAILING" "classifier_failing.png" 1080 1920

# Narrow window shot
take_screenshot "ALIGNED" "narrow.png" 480 900

echo "All screenshots taken."
