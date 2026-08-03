import { useCallback, useState } from "react";
import { EulaGate } from "./components/EulaGate";
import { Hub } from "./components/Hub";

const EULA_KEY = "fc.eula.accepted";
// Bump when the EULA materially changes to re-prompt existing users.
const EULA_VERSION = "2026-07-15";

export default function App() {
  const [accepted, setAccepted] = useState<boolean>(() => {
    try {
      return localStorage.getItem(EULA_KEY) === EULA_VERSION;
    } catch {
      return false;
    }
  });

  const accept = useCallback(() => {
    try {
      localStorage.setItem(EULA_KEY, EULA_VERSION);
    } catch {
      /* ignore */
    }
    setAccepted(true);
  }, []);

  if (!accepted) return <EulaGate onAccept={accept} />;
  return <Hub />;
}
