import type { CapacitorConfig } from "@capacitor/cli";

// The app is a shell around a running Shirley server: it loads the server's page, so `/api`,
// the event stream and the token prompt work exactly as in the browser.
// Set the address when syncing:  SHIRLEY_URL=https://shirley.example.com npm run sync
const url = process.env.SHIRLEY_URL;
if (!url) {
  throw new Error("Set SHIRLEY_URL to the Shirley server address, e.g. SHIRLEY_URL=https://host npm run sync");
}

const config: CapacitorConfig = {
  appId: "dev.shirley.app",
  appName: "Shirley",
  // Bundled copy of the UI; only used if `server.url` is removed.
  webDir: "../crates/shirley/src/interface/web/dist",
  server: {
    url,
    // An http:// address (e.g. a LAN IP while testing) needs this; https needs nothing.
    cleartext: url.startsWith("http://"),
  },
  ios: {
    contentInset: "never",
  },
};

export default config;
