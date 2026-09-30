// Mirrors the Status table in README.md; keep the two in step.
export interface Feature {
  title: string;
  detail: string;
  status: "available" | "planned";
}

export const features: Feature[] = [
  {
    title: "No shared iCloud account",
    detail: "Pair systems directly with a one-time code. No shared Apple ID or Daisy account.",
    status: "available",
  },
  {
    title: "Trust you control",
    detail: "Choose how long each peer stays trusted, or forget it instantly.",
    status: "available",
  },
  {
    title: "Direct encrypted connection",
    detail: "Peers exchange input over an authenticated, encrypted connection.",
    status: "available",
  },
  {
    title: "macOS-native Multi-Touch gestures",
    detail: "Use Spaces, Mission Control and App Exposé from the trackpad in front of you.",
    status: "available",
  },
  {
    title: "Shake mouse pointer to locate",
    detail: "Shake the mouse or trackpad to reveal the pointer on the system with control.",
    status: "available",
  },
  {
    title: "Peer auto-discovery",
    detail: "Daisy finds trusted peers on your network without an address.",
    status: "available",
  },
  {
    title: "Automatic session recovery",
    detail: "Reconnect after sleep, wake and network changes.",
    status: "planned",
  },
  {
    title: "Shared clipboard",
    detail: "Copy on one system and paste on another.",
    status: "available",
  },
  {
    title: "Dynamic chaining",
    detail: "Arrange a whole desk of systems on any edge and move through them as one layout.",
    status: "planned",
  },
];
