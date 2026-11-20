# Shirley for iOS (native)

A SwiftUI app that is the web UI, one to one: the same home list, conversation, task cards,
composer, sheets, skills and connections screens, with the same sizes, colours and wording. It
talks to your running Shirley server over the same endpoints the web page uses (`/api/cmd/*`,
`/api/events`, `/api/upload`); nothing about the server changes.

What is native rather than web:

- **Voice input runs on the phone.** Hold to talk uses iOS speech recognition (on-device where the
  phone supports it); only the recognised text goes to the server.
- Back is the system edge swipe; the file viewer, camera, document scanner, photo and file pickers
  are the system ones.
- Reply notifications are local notifications while the app is open or just backgrounded. The
  connection to the server pauses while the app is suspended, so a reply that finishes then shows
  when you come back, not as a notification (that needs push, which needs APNs on the server).
- 静音 on a bot also silences its reply notifications.
- The address and token are asked for together on first launch and kept (token in the Keychain).
  Long-press the Shirley mark on the home screen to change them.

## Build and install

Needs a Mac with Xcode 26 or later, an Apple ID, and an iPhone on iOS 26 or later (iOS 27 is fine).

1. Open `mobile/native/Shirley.xcodeproj`.
2. Select the **Shirley** target → *Signing & Capabilities* → tick *Automatically manage signing*
   and pick your team. If the bundle identifier `dev.shirley.native` is taken, change it to one of
   your own.
3. Pick your iPhone as the run destination and press **Run** (⌘R). The first install needs the
   developer certificate trusted on the phone (*Settings → General → VPN & Device Management*).
4. On first launch, enter the server address (for example `https://shirley.example.com`, or
   `http://192.168.1.20:8787` on a LAN) and the access token.

The project uses a folder-synced group: any file added under `Shirley/` is part of the app with no
project-file edit.

## Layout

```text
Shirley/
  App/        entry point, notification delegate, AppModel (navigation, reply notifications)
  Core/       Theme (the stylesheet's tokens), Models (wire contract), Session (address + token),
              API (commands, upload), EventHub (the SSE stream), Media (images, compression)
  State/      Stores (bots, tasks), Conversation (port of state/conversation.ts), Replies
  UI/         Chrome (icons, avatars, headers), Markdown, TaskCard, FileViewer, Sheets,
              Composer/ (ComposerDock, SpeechInput)
  Views/      RootView + TokenGateView, HomeView, ChatView, SecondaryViews (new bot, skills,
              connections)
  Assets.xcassets   the web UI's icons, avatars and capability pictures as vector images
```

`Core/Models.swift` and `State/Conversation.swift` mirror the web UI's `api/types.ts` and
`state/conversation.ts`; change them together.
