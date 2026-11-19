import { loginPath } from "../../auth/login/redirect";

let hostSignIn: (() => void) | undefined;

export function installHostSignIn(signIn: () => void): () => void {
  hostSignIn = signIn;
  return () => {
    if (hostSignIn === signIn) hostSignIn = undefined;
  };
}

export function openSignIn(opts?: { redirectTo?: string }): void {
  if (hostSignIn) {
    hostSignIn();
    return;
  }
  if (typeof window === "undefined") return;
  const { pathname, search, hash } = window.location;
  window.location.assign(loginPath(opts?.redirectTo ?? pathname + search + hash));
}
