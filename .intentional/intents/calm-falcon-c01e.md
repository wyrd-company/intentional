---
intentional: patch
---

Treat an empty push trigger filter mapping like a null one when publish derivation adds tag filters, so branch pushes are expanded instead of silently narrowed.
