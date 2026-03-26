# ytplay

`ytplay` is a small Rust CLI that plays audio from a YouTube URL with low overhead by delegating extraction to `yt-dlp` and playback to `mpv`.

## How to use

Install the runtime dependencies with Homebrew:

```bash
brew install yt-dlp mpv
```

Run from the repo with a URL:

```bash
cargo run -- https://www.youtube.com/watch?v=dQw4w9WgXcQ
```

Or run without an argument and paste a URL when prompted:

```bash
cargo run
```

If you want a local release binary:

```bash
cargo build --release
./target/release/ytplay https://www.youtube.com/watch?v=dQw4w9WgXcQ
```

The binary interface is:

```bash
ytplay <url>
```

## Why this design

- Keeps the Rust binary small and simple
- Avoids embedding a downloader, decoder, or browser engine
- Streams audio only, which keeps memory use and startup time low

## Behavior

- Uses `yt-dlp --no-playlist --no-warnings -f bestaudio/best --get-url`
- Launches `mpv` in audio-only mode
- Uses conservative cache settings to keep memory usage low
- Returns mpv's exit code when playback ends

## Scope

`ytplay` v1 handles one URL at a time. It does not support playlists, downloads, search, or queue management.
