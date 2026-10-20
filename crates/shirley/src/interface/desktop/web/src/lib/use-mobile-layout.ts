import { useEffect, useState } from "react";

export const MOBILE_LAYOUT_QUERY = "(max-width: 767px)";

export function useMobileLayout() {
  const [mobile, setMobile] = useState(() => window.matchMedia(MOBILE_LAYOUT_QUERY).matches);
  useEffect(() => {
    const query = window.matchMedia(MOBILE_LAYOUT_QUERY);
    const change = () => setMobile(query.matches);
    change();
    query.addEventListener("change", change);
    return () => query.removeEventListener("change", change);
  }, []);
  return mobile;
}
