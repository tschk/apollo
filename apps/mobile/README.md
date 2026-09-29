# apollo mobile

iOS and Android desk for apollo. One Expo app, the same shape as telekinesis `apps/mobile`.

Crepuscularity does not target phones. It is the GPUI layer `apollo-ui` uses. This UI takes the desktop tokens instead: zinc-950 (`#09090b`) and Chivo Mono.

## What it does

First launch walks the same decisions as the desktop:

1. Provider. ChatGPT, GitHub Copilot, and Claude are pinned. OpenRouter, OpenAI, Gemini, xAI, DeepSeek, Moonshot, Ollama, and a custom OpenAI-compatible endpoint sit in the other list.
2. One folder, or everywhere. Everywhere records `~/.apollo/instances/<id>`.
3. Permission profile: `auto`, `prompt`, `tools_only`, `full`. Same ids as `apollo init`.
4. A test prompt.
5. Simple or advanced.

After that: chat, an instance switcher, and in advanced mode the roster (pin, new instance), the profile, a local log, and the `desktop.json` text.

## Config

Field names match `~/.apollo/desktop.json` from apollo-ui:

- `onboarded`, `mode` (`simple` or `advanced`), `active`
- `instances[]` with `id`, `name`, `everywhere`, `workspace`, `config_dir`, `provider`, `model`, `permission_profile`, `color`, `pinned`

A key is not a field. A key typed here stays in memory for the session so a test prompt or a chat turn can call that provider. OAuth tokens are not collected. Those stay in the rs_ai credential store on the machine running the apollo binary.

A phone has no `~/.apollo`. The document is stored in app storage under `apollo.desktop.json`. Advanced mode prints the JSON. If `~` was used, replace it with that machine's home before dropping the file in place.

A pre-instances file (a single `workspace` and no `instances` array) migrates the same way as the desktop.

## Chat

- An API-key provider is called over HTTPS from this device.
- Ollama is `127.0.0.1:11434` on this device.
- A custom row uses the base URL you typed.
- OAuth, or a key you did not type, returns an offline mock and labels it.

The app does not call `apollo serve`.

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

- iOS and Android binaries are not produced from this Linux tree. There is no iOS toolchain here, and no emulator is started.
- Run it with Expo Go (`npm start`) or the web preview (`npm run web`).
- A session key is not written to disk. After a reload, an API-key chat becomes the offline mock until the key is entered again.
- The permission profile is stored, not enforced. The agent does that.
- Model ids are the desktop defaults. There is no live `/models` fetch.

MPL-2.0, same as the rest of the repo.
