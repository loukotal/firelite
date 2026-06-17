# Auth Emulator API Surface

This tracks the `firebase/auth` client APIs currently used by the app and the emulator-compatible endpoints Firelite needs to support.

## Source files

- `ui/src/app/contexts/firebase/firebase-api.ts`
- `ui/src/app/contexts/firebase/firebase-context.tsx`
- `ui/src/app/contexts/firebase/recaptcha.ts`
- `ui/src/shared/trpc/client.ts`
- `ui/src/partner/utils/use-reload-firebase-for-microsoft-login.ts`
- `ui/src/app/pages/embed-onboarding/embed-onboarding-confirm-details.tsx`

## Client API checklist

| Client API | Expected emulator surface | Status | Notes |
| --- | --- | --- | --- |
| `getAuth` | Client initialization only | implemented | No server endpoint required. |
| `connectAuthEmulator` | Emulator host wiring | implemented | Existing daemon serves Identity Toolkit-style Auth paths. |
| `GoogleAuthProvider` | Provider metadata for `signInWithPopup` | implemented | Covered by tolerant `accounts:signInWithIdp` provider/raw ID contract; official fixture capture still needed. |
| `OAuthProvider` | Generic OAuth provider metadata for `signInWithPopup` | implemented | Covered by tolerant `accounts:signInWithIdp`; Microsoft-specific fixture capture still needed. |
| `signInWithEmailAndPassword` | `accounts:signInWithPassword` | implemented | Covered by current password sign-in contract tests. |
| `signInWithEmailLink` | `accounts:signInWithEmailLink` after OOB code generation | implemented | In-memory single-use OOB code flow; official fixture capture still needed. |
| `isSignInWithEmailLink` | Client URL parsing helper | no server endpoint | Track only to understand email-link workflow assumptions. |
| `sendSignInLinkToEmail` | `accounts:sendOobCode` with `EMAIL_SIGNIN` | implemented | Returns `oobCode` directly and exposes `/emulator/v1/projects/{project}/oobCodes` for local inspection. |
| `signInWithPopup` | IdP sign-in flow via `accounts:signInWithIdp` | implemented | Supports provider ID, raw ID, and email from `postBody`; popup UI behavior still needs SDK/browser discovery. |
| `signInWithCustomToken` | `accounts:signInWithCustomToken` | implemented | Accepts unsigned local JWT-like tokens or plain local IDs for tests. |

## Implementation notes

- Capture official Firebase Auth emulator fixtures before implementing each planned server endpoint.
- Keep client-only helpers in this list so workflow coverage stays visible, even when no Firelite endpoint is required.
- Provider support should start with the providers the app imports: Google and Microsoft-compatible OAuth.
