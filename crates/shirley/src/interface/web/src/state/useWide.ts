import { useEffect, useState } from "react";

const QUERY = "(min-width: 900px)";

/** True on screens wide enough for the two-pane layout (bot list beside the content). */
export function useWide(): boolean {
  const [wide, setWide] = useState(() => window.matchMedia(QUERY).matches);
  useEffect(() => {
    const media = window.matchMedia(QUERY);
    const update = () => setWide(media.matches);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);
  return wide;
}
