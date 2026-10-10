# Contributing / Contribuer

## Branches

```
main      ← releases only (merged from develop)
develop   ← integration branch: every PR targets it
feature/<topic>   new things      e.g. feature/z80-ym2612
fix/<topic>       bug fixes       e.g. fix/vdp-window-plane
```

1. Branch from `develop`: `git switch develop && git pull && git switch -c feature/my-topic`
2. Commit small, explained steps.
3. Open a pull request **into `develop`** and fill in the template.
4. CI must be green (unit tests, differential tests, end-to-end build).
5. Releases: `develop` is merged into `main` and tagged.

## Rules of the house

- **It's a teaching project.** Comment the *why*: the hardware behaviour,
  the trick, the trade-off. A newcomer should learn something by reading
  your diff.
- **Prove CPU changes with the differential test**
  (`python tests/difftest/difftest.py --seed N`). Musashi is the reference.
- **Keep the making-of in sync.** If you change how something works, update
  `docs/index.html` (EN) *and* `docs/fr/index.html` (FR).
- **Never commit ROMs** or any copyrighted game data, not even in tests:
  build test ROMs by hand like `tests/roms/make_test_rom.py`.

---

## En français

- On part toujours de `develop`, dans une branche `feature/<sujet>` ou
  `fix/<sujet>`, et on ouvre une pull request **vers `develop`**.
- `main` ne reçoit que des versions publiées, fusionnées depuis `develop`.
- Commentez le *pourquoi*. Ce projet sert à apprendre.
- Toute modification du CPU se valide avec le test différentiel.
- Le making-of (`docs/`, EN + FR) doit suivre les changements.
- Jamais de ROM ni de données de jeu sous copyright dans le dépôt.
