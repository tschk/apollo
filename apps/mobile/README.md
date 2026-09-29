# apollo mobile

The iPhone and Android app. Same zinc-950 and Chivo Mono as the desktop.

## What you see

First launch:

1. A model. ChatGPT, GitHub Copilot, and Claude first. Then OpenRouter, OpenAI, Gemini, xAI, DeepSeek, Moonshot, Ollama, or an address you type.
2. One folder, or everywhere.
3. What it may do: auto, prompt, tools only, or full.
4. A test prompt.
5. Simple or advanced.

Then chat, and a way to switch instances. Advanced adds the roster, the profile, a log, and the saved setup.

## Setup file

Same fields as `~/.apollo/desktop.json`:

- `onboarded`, `mode` (`simple` or `advanced`), `active`
- `instances[]` with `id`, `name`, `everywhere`, `workspace`, `config_dir`, `provider`, `model`, `permission_profile`, `color`, `pinned`

A key is not one of them. A key you type lasts for this session only.

The phone keeps its own copy, under `apollo.desktop.json`. Advanced shows the text. If you used `~`, put the real home in before using the file elsewhere.

An older file with a single `workspace` and no `instances` list is read the same way as on the desktop.

## Chat

- A key calls that model.
- Ollama is on this phone, at 127.0.0.1:11434.
- A custom row uses the address you typed.
- No key, or an account sign-in: a stand-in, and the screen says so.

## Scripts

```bash
npm test
npm run typecheck
npm start
npm run web
```

Web preview frames, as a hash:

`#welcome` `#provider` `#workspace` `#permissions` `#test` `#mode` `#simple` `#advanced` `#instances`

## Limits

- This tree does not produce an iOS or Android install. Use Expo Go, or the web preview.
- A key is not written down. After a reload, chat is a stand-in until you enter it again.
- The permission choice is saved. The agent is what applies it.
- Model names are the usual defaults. The list is not fetched live.

MPL-2.0, same as the rest of the repo.
