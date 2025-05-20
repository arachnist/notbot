# notbot

notbot, a chatbot that started life in Go, and handling IRC protocol, embraced the riir meme, and is now running on Matrix.

# What is this?

The current iteration of this chatbot started after 38c3, when I've decided to actually try and learn rust. My go-to project for learning a programming language
was always writing a chatbot with little to no regard for feature creep. This iteration exceeds at feature creep and undefined scope.

Bot modules, either commands reacting to specific keywords, or catch-all, can be implemented either natively in Rust (see: [`crate::module`] for the documentation
of module system and a step-by-step guide on what to do), or Lua, thanks to being baited to try and run modules/plugins from
[mun irc bot](https://code.hackerspace.pl/q3k/mun). The result of that was [notmun](https://code.hackerspace.pl/ar/notmun), a slightly modified version of mun, with
some components removed that are no longer needed, that is embedded using the [`mlua`] crate.

Many currently implemented features/modules are tailored for use at the [Warsaw Hackerspace](https://hackerspace.pl/). As such, in the present state it might be
difficult to get it running in a usable state elsewhere.

All modules can be restarted, and configuration can be reloaded without restaring the bot; mun modules - thanks to being written in lua and loaded at runtime from
disk - can have their implementation completely changed at runtime.

Despite using [`matrix_sdk`], additional modules are (for the most part) not implemented as native event handlers. Instead, each module is - essentially - its
own spawned tokio task waiting for messages on the receiving end of a [`tokio::sync::mpsc`] channel.
This was done for several reasons:
* code (de)duplication for common functionality, like matching prefixes, extracting keywords, or even making sure we actually want to process a given event
* implementing unified Acl system for modules
* making sure messages are not processed by multiple modules unintentionally
* compiler errors that are easier to deal with.

# Running notbot

1. Clone [notmun](https://code.hackerspace.pl/ar/notmun) somewhere, eg. `../mun`
2. Copy and edit `notbot.example.toml`. Some parameters are required, and the bot will complain if they're missing, or if syntax is wrong.
3. Set `LUA_PATH='../mun/?;../mun/?.lua'` environment variable properly.
4. Run `RUST_LOG=notbot=info cargo run ./notbot.toml`

# Contributing

Accepting PRs at [hswaw forgejo](https://code.hackerspace.pl/ar/notbot), [github](https://github.com/arachnist/notbot), patches
hurled my way over [Matrix](https://matrix.to/#/@ar:is-a.cat) or [Fedi](https://is-a.cat/@ar) (bait your instance admin to change post
length limit to 20000 😸), or whatever else we figure out and works.

Mind you, this is - still - a very "works on my devices" project. But if you get stuck somewhere, ping me and I can try helping out.

# Future plans

In no particular order:
* more & better docs
* Soon: tests
* actual web interface
* some semi-automatic handling of web interface routes/handlers
* ~~Soon: better ability to map between matrix user ids and hswaw members~~ Done
* feature-gating features, especially things hswaw specific
* using <https://github.com/clarkmcc/cel-rust> for ACLs
* using regular expressions and/or clap for defining module arguments.
* providing more defaults for configuration values, graceful degradation if they're not provided
* Soon: ability to explicitly enable/disable modules at runtime, preferrably in a persistent way
* ingesting notmun modules directly as notbot modules, with a thin rust wrapper around each one.
  right now we're pretending to what remains of mun runtime that there's an irc connection in there somewhere.
* Maybe: resurrecting [dyncfg](https://github.com/arachnist/dyncfg) in some form for dynamic per-room/sender configuration
* removing `notbottime.rs`
* See if we can get around setting `LUA_PATH` env var for mlua
* connecting to jitsi rooms to monitor membership changes on them. might use a Go sidecar (prototype [here](https://github.com/arachnist/jitsi-go/)) for that, as it was easier to modify a [Go xmpp](https://github.com/arachnist/go-xmpp) library to speak jitsi, than modifying [xmpp-rs](https://docs.rs/xmpp/latest/xmpp/) to do the same.
* some sort of fedi integration. did i mention there's no scope defined?
