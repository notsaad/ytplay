# ytplay

`ytplay` is a small Rust CLI that plays audio from a YouTube URL with low overhead by delegating extraction to `yt-dlp` and playback to `mpv`.

## How to use

Install the runtime dependencies with Homebrew:

```bash
brew install yt-dlp mpv
```

Run from the repo with a URL:

```bash
cargo run -- 'https://www.youtube.com/watch?v=dQw4w9WgXcQ'
```

After playback starts, `ytplay` clears the terminal and shows:

- the video title
- a progress bar with elapsed and total time
- keyboard controls for playback and audio

Or run without an argument and paste a URL when prompted:

```bash
cargo run
```

Or pipe the URL on stdin:

```bash
printf '%s\n' 'https://www.youtube.com/watch?v=dQw4w9WgXcQ' | cargo run
```

If you want a local release binary:

```bash
cargo build --release
./target/release/ytplay 'https://www.youtube.com/watch?v=dQw4w9WgXcQ'
```

The binary interface is:

```bash
ytplay <url>
```

If you are using `zsh`, quote full YouTube URLs on the command line. Characters like `?` and `&` are interpreted by the shell before `ytplay` sees them.

## Controls

While playback is active in an interactive terminal:

- `P` toggles play and pause
- `U` lowers volume
- `I` raises volume
- `M` toggles mute
- `Q` quits playback

## Why this design

- Keeps the Rust binary small and simple
- Avoids embedding a downloader, decoder, or browser engine
- Streams audio only, which keeps memory use and startup time low

## Behavior

- Uses `yt-dlp --no-playlist --no-warnings -f bestaudio/best --get-url`
- Launches `mpv` in audio-only mode
- Uses mpv's JSON IPC to render a lightweight in-terminal playback UI
- Uses conservative cache settings to keep memory usage low
- Returns mpv's exit code when playback ends

## Scope

`ytplay` v1 handles one URL at a time. It does not support playlists, downloads, search, or queue management.
