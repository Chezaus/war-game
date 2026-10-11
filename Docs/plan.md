# War Game — Basic Prototype Plan

> Architecture: [architect.md](architect.md). Section references like "§5" point there.

## Rules
- Seven steps, **P0–P6**. Each step ends in something you can run and try.
- Sizes: **S** ≤ ½ day, **M** 1–2 days. Nothing larger: split it.
- Put every number in `config.rs`. Hardcoding is fine.
- Don't start the next step until the current step's **done when** passes.
- Tick tasks off as you go (`[ ]` → `[x]`).

| Step | Result | Approx. effort |
|------|--------|----------------|
| P0 | Window, states, debug camera | 1 day |
| P1 | Terrain, sea, airbases, bridge | 2–3 days |
| P2 | Fly a jet (takeoff, G-force, fuel, crash, land) | 4–5 days |
| P3 | Guns, missiles, flares, explosions | 3–4 days |
| P4 | Enemy and wingman AI jets | 3–4 days |
| P5 | Map mode + ground units | 4–5 days |
| P6 | Game loop: reserve jets, restock, win/lose, sound | 2–3 days |
| P7 | Infrastructure: power plants, fuel depots, radars, strike raids | 2–3 days |
| P8 | Infantry: soldier squads, play on foot, IR drone | 4–5 days |
| P9 | Mini Earth: roads, forests, bridges, auto-advancing units, warheads | 4–5 days |

---

## P0 — Skeleton
- [x] **S** Create the Cargo binary at the repo root. Pin an exact Bevy version. Add `rand`, and `bevy_egui` if it supports that Bevy version. Fast dev build profile (optimize dependencies).
- [x] **S** `git init` + `.gitignore`.
- [x] **S** `state.rs`: Menu → Playing → GameOver. Pause toggle on Esc. View mode: Flight / Map / DeathCam.
- [x] **S** Register empty plugins for every file in §9.
- [x] **S** `debug.rs`: free-fly camera, FPS counter.

**Done when:** the game opens to a menu, "Start" enters Playing, and you can fly the debug camera around an empty scene.

## P1 — World
- [x] **M** Make `assets/heightmap.png`: a 64 km coast with hills, sea on one side, a river and two flat runway areas. Paint it by hand or derive it loosely from Baltic coast elevation.
- [x] **S** Load the heightmap. Height lookup + is-water functions, with unit tests.
- [x] **M** Build 8×8 flat-shaded terrain chunk meshes colored by height.
- [x] **S** Sea plane, sky, distance fog, sun light.
- [x] **S** Two airbases (runway strip, hangar boxes, base zone) and the bridge model, at positions from `config.rs`.

**Done when:** the debug camera shows the coast, sea, river, bridge and both airbases at 60 FPS.

## P2 — Flight
- [x] **S** `config.rs`: NATO and enemy jet stats (§5.1).
- [x] **S** Low-poly jet mesh from primitives.
- [x] **M** Flight model: atmosphere, thrust, lift with stall, drag, gravity. Unit tests (level flight holds, stall drops lift).
- [x] **M** Rate-commanded rotation scaled by airspeed + G-limiter. Keyboard/mouse controls (§7).
- [x] **M** Ground contact: wheels on runway/terrain, brakes, takeoff roll, landing, crash rules.
- [x] **S** Chase and cockpit cameras (V toggle), cockpit free-look.
- [x] **M** HUD: speed, altitude, heading, G, throttle, fuel, pitch line.
- [x] **S** G-force tolerance → grey-out/blackout/red-out vignette with control fade.
- [x] **S** Fuel burn, engine-out, fuel leak (debug key) with a HUD warning.
- [x] **S** Debug: velocity/lift arrows, egui sliders for aero constants.

**Done when:** you can take off from the NATO runway, loop, stall and recover, black out at 9 G, land again, and crash into a hill. The flight model feels good. **Spend extra time tuning here.**

## P3 — Weapons and explosions
- [x] **S** Jet HP, engine damage, fuel tank hit (leak), fire (§5.2).
- [x] **M** Cannon: projectiles, segment hit checks against hit-spheres and terrain, tracers.
- [x] **M** Heat-seeking missile: seeker cone, lock + tone, proportional navigation, burn/coast, proximity fuse. HUD target box + lock indicator.
- [x] **S** Flares + decoy chance. Missile warning tone and HUD arrow.
- [x] **M** Detonation message → radius damage + explosion FX (pooled particles, light flash, camera shake, debris). Missile smoke trails.
- [x] **S** Debug: a stationary target drone and a cheat key to spawn one.

**Done when:** you can shoot down target drones with guns and missiles, explosions look big and satisfying, and a hit leaves your own jet leaking fuel.

## P4 — AI jets
- [x] **M** Pilot AI state machine: Patrol → Engage → Evade → Return to base, plus terrain avoidance (§5.5). It uses the same controls as the player.
- [x] **S** Enemy jet spawner: keep 2–4 enemy jets airborne from the enemy base, with a cooldown.
- [x] **S** One friendly AI flight (2 jets) patrolling near the NATO base.
- [x] **S** Death: player jet destroyed → 3 s death cam → placeholder respawn.

**Done when:** 1v2 dogfights are fun and fair. AI jets dodge missiles with flares, rarely crash into terrain, and can kill you.

## P5 — Map and ground units
- [x] **S** `config.rs`: ground unit types (infantry, armor, anti-air) and starting positions (6 NATO, 8 enemy).
- [x] **M** Ground units: position, strength, movement snapped to terrain, bridge-only river crossing, visual model clusters that shrink with strength.
- [x] **S** Auto-combat once per second + retreat below 25 strength (§6.2). Unit test for the combat formula.
- [x] **S** Strafing and explosions damage ground unit strength. Bridge has HP and can be destroyed.
- [x] **S** Anti-air units fire real missiles at enemy jets in range (including at the player).
- [x] **S** Enemy script: two waves advancing on the NATO base via the bridge. The second wave needs the bridge intact.
- [x] **M** Map mode (M): top-down camera, pan/zoom, icons with strength bars, battle icons, autopilot holds level flight while open.
- [x] **M** Selection + right-click orders for ground units and the friendly flight. Path lines. egui side panel with unit info + event log.

**Done when:** you can open the map, send your armor to the bridge, fly over and strafe an enemy column, dodge AA missiles, blow the bridge, and watch the battle play out on the map.

## P6 — Game loop and polish
- [x] **S** Jet reserve (4): death → map; "Launch Jet" button spawns you on the runway. Every loss counts.
- [x] **S** Restock: stop on your runway for 5 s → refuel/rearm/repair with a HUD progress bar (§5.4).
- [x] **S** Base capture rule, win/lose conditions, end screen with stats (§6.6).
- [x] **S** Sounds: engine, gun, missile, explosion, lock and warning tones.
- [x] **S** Main menu: start, controls list, quit.
- [ ] **M** Balance pass: unit strengths, enemy jet count, AA chance, missile hit chance. Play 3 full rounds and fix the worst issues. *(Only automated soak runs so far; needs real play-testing.)*

**Done when:** a full 15–30 minute round can be won or lost, and it's fun to switch between flying and the map.

## P7 — Infrastructure (§6.4)
- [x] **S** `config.rs`: site types (power plant, fuel depot, radar), HP, positions (3 per side), effect factors.
- [x] **M** `infrastructure.rs`: low-poly site models, hit sphere relayed to the site, HP, rubble + smoke when destroyed.
- [x] **S** Effects: power feeds radar and fuel. Radar down → AA fires less, AI jets detect at half range, NATO map only shows enemy jets near your forces. Fuel down → NATO restock 2.5× slower, enemy jet launches 2.5× rarer.
- [x] **S** Ground units with no enemy unit in range shell enemy sites in range.
- [x] **M** Enemy strike raids: every ~2.5 min an enemy fighter dives on a NATO site and fires 2 guided air-to-ground missiles at it (new AI `Strike` state). Event log warning + dashed line on the map.
- [x] **S** Map: site icons with HP bars, infrastructure list and effect warnings in the side panel. End-screen stats. Unit tests (effects, sites on land).

**Done when:** you can fly over and destroy the enemy radar and see their AA weaken, an enemy raid can knock out your fuel depot and slow your restock, and the map shows it all.

## P8 — Infantry (§6.5)
- [x] **S** Infantry units are drawn as squads of individually modeled soldiers (foot speed).
- [x] **M** Infantry mode: select a NATO infantry unit, press I (or the panel button) to fight on foot. First-person walk/sprint/crouch on the heightmap, bridge deck, no swimming. M returns to the map.
- [x] **M** Rifle: tracers through the shared bullet code, magazine + reload, aim zoom, resupply near friendly units. Hits on unit models cost the unit strength.
- [x] **M** Threats: enemy units in range with terrain line of sight shoot at you once a second (crouching and cover help). Enemy recon drones hunt you; crouch and keep still to stay unspotted, or shoot them down. Death → death cam → map, and the squad loses strength.
- [x] **M** IR drone (T): fly it with WASD/Q/E, enemy troops and vehicles glow, drop 2 small bombs. Battery and recharge.
- [x] **S** Infantry HUD (HP, stance, squad, ammo, drone, SPOTTED / UNDER FIRE warnings). Unit tests (hit chance, drone spotting).
- [ ] **S** Play-test pass: enemy hit chance, drone bomb damage, soldier speed. *(Only automated runs so far.)*

**Done when:** you can join a squad at the front, use the drone to find a hidden enemy column, bomb it, survive by crouching behind a hill, and go back to the map.

---

## P9 — Mini Earth, auto-advance, warheads (§4, §6.2, §6.7)
- [x] **M** `geo.rs`: projection with Europe magnified; coastlines, islands, inland seas, ~40 rivers and straits, mountain ranges, deserts, cities and roads in lon/lat. Unit tests (projection round trip, world fits).
- [x] **M** `heightgen.rs`: 128 km, 1025² heightmap from a coastline distance field, ridges, river valleys, straits, flat runways. Preview images (`target/heightmap_preview.png`, `target/europe_preview.png`).
- [x] **S** Bridges wherever a road crosses ≤ 2 km of water: arched decks, pillars, HP each; map icons and panel list.
- [x] **S** Road ribbons; climate-coloured terrain; ~60k merged trees with a view range.
- [x] **M** `nav.rs`: 250 m grid, A* with road/forest costs, bridge-aware connected regions, path smoothing that keeps to roads and bridge decks. Unit test (Belarus → Poland, Paris → London over the Channel bridge, none once bridges fall).
- [x] **M** Auto orders: idle units attack the nearest reachable enemy (else base, else sites); AA escorts; right-click order → Auto again 30 s after arrival; Hold. Re-plan when bridges fall.
- [x] **S** Forests: vehicles slower, infantry units take less damage, soldier on foot harder to hit and spot.
- [x] **M** Ballistic warheads: silos in America and Siberia, map targeting, visible arc, mushroom-cloud blast, enemy launches at NATO clusters with a warning.
- [ ] **S** Play-test pass: warhead damage/count, auto-advance pace, road speed, forest factors. *(Only automated soak runs so far.)*

**Done when:** your troops and the enemy's advance on their own along roads and over bridges, you can blow a bridge and watch them re-route or stall, fire a warhead from America onto an enemy column, and scatter your units before the enemy's lands.

## After the prototype
If it's fun, grow toward the full vision in this order (see architect.md §11): carrier ops → simple budget → fog of war → hybrid LOD for the soldiers → real theaters. (Infrastructure and infantry mode were pulled forward as P7 and P8.)
