# gase-recomp

**Une ROM en entrée, un jeu natif en sortie.** gase-recomp est un
recompilateur statique *pédagogique* pour les jeux Sega Mega Drive /
Genesis. Apportez une ROM qui vous appartient : le code machine 68000 du jeu
est traduit en Rust, qui tourne sur les puces de
[**gase**](https://github.com/bthuner/gase), un émulateur Mega Drive en Rust.
Seul le 68000 est remplacé par du code recompilé ; VDP, YM2612, PSG et Z80
sont ceux de gase.

> **Deux éditions cohabitent.** L'**édition Rust** (`crates/`) est la voie
> principale. La première **édition C + Python** (`gase/`, `runtime/`,
> `tests/`) reste en référence pédagogique : même pipeline, autres
> compromis. Le making-of raconte pourquoi nous avons changé.

L'objectif est autant d'**apprendre à construire un projet de décompilation /
recompilation** que de faire tourner des jeux. Chaque module est court,
commenté, et correspond à une étape du pipeline. Le making-of l'explique
chapitre par chapitre :
**[bthuner.github.io/gase-recomp/fr](https://bthuner.github.io/gase-recomp/fr/)**
(sources dans [`docs/`](docs/)).

🇬🇧 [Read in English](README.md)

```
 jeu.md ──► rom.py ──► m68k/decoder.py ──► analysis.py ──► codegen.py ──► project.py
 (ROM)     en-tête,    code machine        fonctions,      un bloc C par   CMake + SDL2
           vecteurs    68000 → objets      branches, appels instruction    desktop/android/ios
                                                                │
                             runtime/ (C) : CPU, carte mémoire, VDP, PSG, manettes
```

## Démarrage rapide (édition Rust)

Prérequis : Rust 1.85+ ; pour une fenêtre, SDL2 (`apt install libsdl2-dev`,
`brew install sdl2`).

```sh
cargo run --release -p gase-recomp -- info   jeu.md
cargo run --release -p gase-recomp -- recomp jeu.md -o build/jeu
cd build/jeu && cargo run --release            # la fenêtre, les menus et les sauvegardes de gase
cargo run --release -- --headless --frames 600 --screenshot capture.png
```

Le dossier généré est un crate Cargo ordinaire : `src/recomp/` contient le
code du jeu en Rust, `rom.bin` votre ROM. Le programme **est gase**,
l'émulateur, avec un processeur de plus : il enregistre le code recompilé
pour le CRC-32 de sa ROM, puis lance l'interface de gase. Fenêtre, menus,
sauvegardes d'état avec aperçus, retour arrière, manettes et réglages sont
donc ceux de gase. Le code que l'analyse n'a pas trouvé tourne sur
l'interpréteur 68000 de gase : le jeu continue, et l'adresse est notée dans
`gase_missing.txt` ; relancez `recomp` et elle devient du Rust à son tour.

Ordinateur (Linux, macOS, Windows), mode sans fenêtre, **Android et iOS**.
Chaque jeu généré a aussi un crate `mobile/` : le même jeu en application
de téléphone, avec son propre `SDL_main`, la ROM intégrée, et les contrôles
tactiles et menus de gase. Les projets Android et iOS de gase le
construisent (les commandes sont dans le `BUILD.md` du jeu ; la CI
construit l'APK de la ROM de test). Le navigateur viendra ensuite.

### Le studio : choisir une ROM, appuyer sur Build, jouer

```sh
cargo run --release -p gase-recomp-studio            # la fenêtre
cargo run --release -p gase-recomp-studio -- --check # ce qui est installé, ce qui manque
cargo run --release -p gase-recomp-studio -- --cli jeu.md    # pareil, sans fenêtre
cargo run --release -p gase-recomp-studio -- --cli jeu.md --target desktop --run
```

**This computer** : recompiler, compiler, puis **Play** lance gase avec le
code du jeu intégré (`--desktop jeu.md` ouvre la fenêtre réglée ainsi).
**Android app** : une fenêtre autour de toute la construction Android. Choisissez une ROM,
vérifiez le nom et l'identifiant de l'application, appuyez sur **Build the
APK**. Le studio recompile la ROM, récupère gase à la bonne révision,
construit avec le projet Android de gase et dépose `<jeu>.apk` à côté du
projet, avec un bouton pour l'installer sur un téléphone en USB (`adb`). La
liste de droite montre ce qui manque à l'ordinateur (SDK Android, NDK, Java,
Gradle, CMake, cible Android de Rust) et corrige ce qu'une commande peut
corriger. Linux et macOS ; sous Windows, passez par WSL.

```sh
cargo test --workspace     # décodeur, tests différentiels, bout en bout
```

## Démarrage rapide (édition C, référence pédagogique)

Prérequis : Python 3.10+, un compilateur C, CMake, SDL2 (`apt install libsdl2-dev`,
`brew install sdl2`, vcpkg sous Windows).

```sh
python -m gase info    jeu.md                  # en-tête, checksum, vecteurs
python -m gase disasm  jeu.md | less           # ce que l'analyse a trouvé
python -m gase recomp  jeu.md -o build/jeu     # demande la plateforme
cmake -S build/jeu -B build/jeu/out && cmake --build build/jeu/out
./build/jeu/out/<nom>                          # touches : flèches, Z X C, Entrée
```

`--platform desktop|android|ios|headless` évite la question. Chaque dossier
généré contient un `BUILD.md` avec les étapes propres à la plateforme
(Android Studio, Xcode).

### La boucle de couverture

L'analyse statique ne peut pas savoir où mène un `jmp (a0)`. Quand le jeu
atteint du code non recompilé, le runtime écrit l'adresse dans
`gase_missing.txt` et s'arrête. Placez ce fichier dans le dossier généré,
relancez la même commande `recomp`, recompilez : l'adresse est devenue une
fonction. On répète jusqu'à ce que le jeu se joue de bout en bout.

> Aucune donnée de jeu n'est incluse ni distribuée. Utilisez vos propres ROM.

## Visite du dépôt

| chemin | ce qu'on y apprend |
|--------|--------------------|
| [`gase/rom.py`](gase/rom.py) | structure d'une ROM : vecteurs, en-tête Sega, checksum, format SMD |
| [`gase/m68k/`](gase/m68k/) | décoder un jeu d'instructions CISC : lignes d'opcodes, modes d'adressage |
| [`gase/analysis.py`](gase/analysis.py) | désassemblage récursif, fonctions, tables de sauts |
| [`gase/codegen.py`](gase/codegen.py) | traduire les instructions en C, labels/goto, appels et retours |
| [`runtime/include/gase/gase.h`](runtime/include/gase/gase.h) | le contrat entre le code généré et le runtime |
| [`runtime/include/gase/m68k_ops.h`](runtime/include/gase/m68k_ops.h) | les flags du 68000 (X N Z V C), décalages, BCD, mul/div |
| [`runtime/src/cpu.c`](runtime/src/cpu.c) | dispatch, exceptions, interruptions |
| [`runtime/src/system.c`](runtime/src/system.c) | l'ordonnanceur par ligne : comment du code précompilé reçoit des interruptions |
| [`runtime/src/vdp.c`](runtime/src/vdp.c) | la puce vidéo : plans, sprites, scrolling, DMA |
| [`runtime/src/psg.c`](runtime/src/psg.c) | la puce son SN76489 |
| [`runtime/src/platform_sdl.c`](runtime/src/platform_sdl.c) | une seule coque SDL2 pour ordinateur, Android et iOS (contrôles tactiles) |
| [`tests/difftest/`](tests/difftest/) | tests différentiels contre l'émulateur 68000 Musashi |
| [`tests/roms/make_test_rom.py`](tests/roms/make_test_rom.py) | une ROM Mega Drive écrite à la main pour les tests de bout en bout |

## Tests

```sh
pip install pytest machine68k
python -m pytest
python tests/difftest/difftest.py --cases 2000 --seed 7
```

## État et feuille de route

Premier vrai jeu : **hashbug**, un homebrew de 2,2 Mo, recompilé en une
passe et joué dans gase avec des graphismes identiques à l'émulateur (build
release d'environ 5 minutes).
Deuxième : **Aladdin**, recompilé en une passe (build release en moins de
4 minutes) et entièrement jouable. Quelques défauts de sprites et de décor,
absents de l'émulateur, venaient du temps : des durées d'instructions
estimées (maintenant mesurées sur le 68000 de gase et vérifiées au cycle
près par le test différentiel) et des attentes matérielles comptées trop
tard (maintenant comptées tout de suite). Les défauts ne se reproduisent
plus (making-of, chapitre 10). Un interrupteur de bissection,
`GASE_RECOMP_INTERPRET`, aide à trouver une routine mal traduite dans un
jeu qu'on n'a pas.

Fonctionne aujourd'hui :
- le jeu d'instructions 68000 complet, avec le décodeur comparé à Musashi sur les 65 536 opcodes ;
- le VDP (plans, fenêtre, sprites, scrolling, DMA), le PSG et les manettes 3 boutons ;
- les interruptions VBlank/HBlank et les builds desktop.

La génération des projets Android et iOS est en place, mais pas encore testée sur appareil.

Prochaines étapes (autant de bonnes branches `feature/*`) : Z80 + YM2612,
sauvegardes SRAM, manettes 6 boutons, mode shadow/highlight, un interpréteur
de secours pour le code en RAM, un modèle de cycles plus fin, les ROM de
plus de 4 Mio.

## Contribuer

`main` contient les versions publiées et `develop` sert de branche d'intégration. Tout le travail
se fait dans des branches `feature/*` ou `fix/*`, fusionnées dans `develop`
par pull request. Voir [CONTRIBUTING.md](CONTRIBUTING.md).

## Licence

MIT, © 2026 Théo Hunerblaes. Sega, Genesis et Mega Drive sont des marques de
Sega ; ce projet n'est pas affilié à Sega.
