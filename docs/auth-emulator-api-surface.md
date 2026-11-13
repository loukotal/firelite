# Auth Emulator API Surface

This tracks the `firebase/auth` client APIs used by a reference consumer application and the emulator-compatible endpoints Firelite needs to support. The source paths below are relative to that consumer checkout, not this repository.

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
| `GoogleAuthProvider` | `accounts:signInWithIdp` with `providerId=google.com` | implemented | `GoogleAuthProvider.credential` accepts fake claims as JSON or an unsigned JWT; responses carry Google `federatedId`, `rawUserInfo`, and profile fields. Covered by contract tests and the Firebase Web SDK harness. |
| `OAuthProvider` | Generic OAuth provider metadata for `signInWithPopup` | implemented | Uses the same `accounts:signInWithIdp` claims flow, with raw claims as `rawUserInfo`; Microsoft-specific fixture capture still needed. |
| `signInWithEmailAndPassword` | `accounts:signInWithPassword` | implemented | Covered by current password sign-in contract tests. |
| `signInWithEmailLink` | `accounts:signInWithEmailLink` after OOB code generation | implemented | In-memory single-use OOB code flow; official fixture capture still needed. |
| `isSignInWithEmailLink` | Client URL parsing helper | no server endpoint | Track only to understand email-link workflow assumptions. |
| `sendSignInLinkToEmail` | `accounts:sendOobCode` with `EMAIL_SIGNIN` | implemented | Returns `oobCode` directly and exposes `/emulator/v1/projects/{project}/oobCodes` for local inspection. |
| `sendEmailVerification` / `applyActionCode` | `accounts:sendOobCode` with `VERIFY_EMAIL`, then `accounts:update` | implemented | Generates a single-use OOB code, marks the user verified when applied or when the emulator action link is opened, and prints that link to the console. |
| `signInWithPopup` / `signInWithRedirect` | `/emulator/auth/handler` account picker, `/emulator/auth/iframe` relay, then `accounts:signInWithIdp` | implemented | Checked by hand in Chrome with the Firebase Web SDK. The relay iframe loads `gapi` from apis.google.com. Redirect needs the app and emulator on the same site. |
| `signInWithCredential` / `linkWithCredential` (IdP) | `accounts:signInWithIdp` with optional `idToken` | implemented | Linking rejects identities already linked elsewhere (`FEDERATED_USER_ID_ALREADY_LINKED`) and emails owned by another user (`EMAIL_EXISTS`). |
| `signInWithCustomToken` | `accounts:signInWithCustomToken` | implemented | Accepts unsigned local JWT-like tokens or plain local IDs for tests. |
| `PhoneAuthProvider.verifyPhoneNumber` | `accounts:sendVerificationCode` and `accounts:signInWithPhoneNumber` | implemented | Generates inspectable, single-use SMS codes exposed by `/emulator/v1/projects/{project}/verificationCodes` and printed to the console. |
| `updatePhoneNumber` | `accounts:update` with `phoneVerificationInfo` | implemented | Links the verified phone to the ID token's existing user without replacing its identity or claims. |
| `multiFactor` phone enrollment | v2 `accounts/mfaEnrollment:start` and `accounts/mfaEnrollment:finalize` | implemented | Uses inspectable, single-use emulator SMS codes and persists factors in `mfaInfo`. |
| `getMultiFactorResolver` phone sign-in | Password first factor plus v2 `accounts/mfaSignIn:start` and `accounts/mfaSignIn:finalize` | implemented | Password login returns `mfaPendingCredential`; completed and refreshed tokens retain phone second-factor claims. |
| `RecaptchaVerifier` emulator discovery | v2 `recaptchaConfig` and v1 `recaptchaParams` | implemented | Returns the fake emulator site key with phone and email/password enforcement disabled. |

## Implementation notes

- Capture official Firebase Auth emulator fixtures before implementing each planned server endpoint.
- Keep client-only helpers in this list so workflow coverage stays visible, even when no Firelite endpoint is required.
- Provider support should start with the providers the app imports: Google and Microsoft-compatible OAuth.
- Project-less Auth client endpoints use the project configured at emulator startup; Admin and emulator-management routes continue to use their explicit project path.
