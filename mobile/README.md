# Shirley mobile

The iOS app (Android later, in this same folder): a native shell, made with
[Capacitor](https://capacitorjs.com), that loads your running Shirley server's page. `/api`,
the event stream, the token prompt and voice input work as in the browser; nothing about the
server changes.

## Install on your iPhone over USB

Needs a Mac with Xcode, an Apple ID, and a Shirley server the phone can reach — preferably over
**https** (the microphone needs it in the browser; the app allows `http://` too, for a LAN IP
while testing).

```sh
cd mobile
npm install
SHIRLEY_URL=https://your-shirley-host npm run ios   # builds the web UI, syncs, opens Xcode
```

In Xcode:

1. Select the **App** target → *Signing & Capabilities* → tick *Automatically manage signing*
   and pick your Apple ID as *Team* (Xcode → Settings → Accounts to add it). If Xcode says
   the bundle identifier is taken, change `dev.shirley.app` to something of your own.
2. Plug in the iPhone, trust the computer, and pick it as the run destination.
3. On the phone turn on *Settings → Privacy & Security → Developer Mode* (it restarts).
4. Press **Run** (⌘R). The first time, the phone refuses to open the app: *Settings → General →
   VPN & Device Management* → trust your developer certificate, then open it again.

The app asks for the access token on first launch, like the web page, and remembers it.

With a free Apple ID the install expires after **7 days**: plug in and press Run again. A paid
developer account makes it last a year and allows TestFlight.

## Changing the server address

`SHIRLEY_URL` is written into the app at sync time. To point at another server:
`SHIRLEY_URL=https://other-host npm run sync`, then Run in Xcode again. UI changes need no
rebuild of the app: it loads the page from the server.

## What does not work yet

- Reply notifications: the iOS web view has no web notifications; real push needs APNs.
- The browser speech recogniser fallback (used without `whisper.cpp` on the server) does not
  exist in the web view; native iOS speech recognition is the next step.
