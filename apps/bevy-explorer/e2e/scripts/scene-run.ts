// Runs the scenes of an SDK7 test-scene workspace one after another in one client: serves every
// built scene folder under a root as one loopback realm, boots a guest, then for each scene that
// has a command file teleports to its base parcel, runs the file's console lines and keeps what
// happened.
//
//   node scripts/scene-run.ts --scenes <root> --commands <dir> [--scene <folder> ...] [--report <dir>] [--web-app <dir>]
//
// A scene folder is a direct child of the root holding `scene.json` and its built `main` bundle;
// folders without the bundle are listed as not built and left out of the realm. A scene runs when
// `<commands dir>/<folder>.txt` exists (`--scene` keeps only the named folders). The command file
// has the drive tool's form: one console line each, blank and `#` lines skipped. For each run
// `<report>/<folder>/` gets `result.json` (`scene`, `title`, `parcel`, `arrival`, `commands`,
// `logs`) and `final.png`; `logs` is every line the client keeps of the scene under the player at
// the end (`/scene_logs --since 0`). stdout carries one JSON summary line per scene, stderr the
// progress. Report folder `report/scene-run` by default.
import { access, mkdir, readdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { type SceneLogPage } from '../driver/explorer.ts'
import { PACKAGE_ROOT, REPORT_DIR } from '../driver/paths.ts'
import { buildScene, serveRealm } from '../driver/realm.ts'
import { closeSessions, openSession, parsePosition } from '../driver/session.ts'
import { readTarget } from '../driver/target.ts'
import { type Result, runLines } from './drive.ts'

export interface Options {
  root: string
  commands: string
  only: string[]
  report: string
  webApp?: string
}

export interface SceneResult {
  scene: string
  title?: string
  parcel: string
  /** the teleport's reply, or why it failed */
  arrival: { ok: boolean; sceneState?: string; settled?: boolean; error?: string }
  commands: Result[]
  logs: SceneLogPage | { error: string }
  final?: string
}

export function parseArgs(argv: string[], cwd: string = process.cwd()): Options {
  let root: string | undefined
  let commands: string | undefined
  const options: Omit<Options, 'root' | 'commands'> = { only: [], report: path.join(REPORT_DIR, 'scene-run') }
  const value = (i: number, name: string): string => {
    const next = argv[i]
    if (next === undefined || next.startsWith('--')) throw new Error(`${name} needs a value`)
    return next
  }
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]!
    // `pnpm run scene-run -- --scenes ...` hands the separator on
    if (arg === '--') continue
    if (arg === '--scenes') root = path.resolve(cwd, value(++i, arg))
    else if (arg === '--commands') commands = path.resolve(cwd, value(++i, arg))
    else if (arg === '--scene') options.only.push(value(++i, arg))
    else if (arg === '--report') options.report = path.resolve(cwd, value(++i, arg))
    else if (arg === '--web-app') options.webApp = path.resolve(cwd, value(++i, arg))
    else throw new Error(`unknown argument ${arg}`)
  }
  if (!root) throw new Error('--scenes <root> is needed')
  if (!commands) throw new Error('--commands <dir> is needed')
  return { root, commands, ...options }
}

async function exists(file: string): Promise<boolean> {
  return access(file).then(
    () => true,
    () => false
  )
}

/** The scene folders directly under `root`, sorted: `built` hold `scene.json` and its `main`
 *  bundle, `unbuilt` only `scene.json`. */
export async function findScenes(root: string): Promise<{ built: string[]; unbuilt: string[] }> {
  const built: string[] = []
  const unbuilt: string[] = []
  const entries = await readdir(root, { withFileTypes: true })
  for (const entry of entries.filter((entry) => entry.isDirectory()).sort((a, b) => a.name.localeCompare(b.name))) {
    const dir = path.join(root, entry.name)
    let metadata: { main?: unknown }
    try {
      metadata = JSON.parse(await readFile(path.join(dir, 'scene.json'), 'utf8')) as { main?: unknown }
    } catch {
      continue
    }
    const main = typeof metadata.main === 'string' ? metadata.main : 'bin/index.js'
    ;((await exists(path.join(dir, main))) ? built : unbuilt).push(entry.name)
  }
  return { built, unbuilt }
}

/** The built folders that run, in order: those with a command file (`<folder>.txt`), narrowed to
 *  `only` when given. `missing` names what was asked for but cannot run, with the reason. */
export function selectRuns(built: string[], commandFiles: string[], only: string[]): { runs: string[]; missing: string[] } {
  const withCommands = new Set(commandFiles.filter((file) => file.endsWith('.txt')).map((file) => file.slice(0, -'.txt'.length)))
  const missing = only.flatMap((folder) =>
    !built.includes(folder) ? [`${folder}: not a built scene folder`] : !withCommands.has(folder) ? [`${folder}: no ${folder}.txt`] : []
  )
  const runs = built.filter((folder) => withCommands.has(folder) && (only.length === 0 || only.includes(folder)))
  return { runs, missing }
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

async function* linesOf(text: string): AsyncIterable<string> {
  yield* text.split(/\r?\n/)
}

async function main(): Promise<void> {
  const options = parseArgs(process.argv.slice(2))
  const read = readTarget()
  const target = { ...read, webApp: options.webApp ?? read.webApp }
  const { built, unbuilt } = await findScenes(options.root)
  if (unbuilt.length > 0) console.error(`[scene-run] ${unbuilt.length} scene folders are not built and left out`)
  const { runs, missing } = selectRuns(built, await readdir(options.commands), options.only)
  for (const reason of missing) console.error(`[scene-run] skipped ${reason}`)
  if (runs.length === 0) throw new Error('no built scene folder has a command file')

  const scenes = new Map(await Promise.all(built.map(async (folder) => [folder, await buildScene(path.join(options.root, folder))] as const)))
  const base = (folder: string): string => {
    const metadata = scenes.get(folder)!.entity.metadata as { scene?: { base?: string } }
    return metadata.scene?.base ?? scenes.get(folder)!.entity.pointers[0]!
  }
  const realm = await serveRealm({ scenes: [...scenes.values()], port: target.ports.realm })
  console.error(`[scene-run] realm ${realm.realm}: ${built.length} scenes, running ${runs.join(' ')}`)
  try {
    const { explorer } = await openSession({ realm: realm.realm, position: base(runs[0]!), signIn: 'guest' }, target)
    for (const folder of runs) {
      const parcel = base(folder)
      const report = path.join(options.report, folder)
      await mkdir(report, { recursive: true })
      const result: SceneResult = { scene: folder, parcel, arrival: { ok: false }, commands: [], logs: { error: 'not read' } }
      try {
        const [x, y] = parsePosition(parcel)
        const { teleport } = await explorer.teleport(x, y)
        result.arrival = { ok: teleport.arrived, sceneState: teleport.sceneState, settled: teleport.settled }
        result.title = teleport.title
      } catch (error) {
        result.arrival = { ok: false, error: message(error) }
      }
      console.error(`[scene-run] ${folder} at ${parcel}: ${result.arrival.ok ? `arrived, ${result.arrival.sceneState}` : `no arrival (${result.arrival.error ?? result.arrival.sceneState})`}`)
      if (result.arrival.ok) {
        const commands = await readFile(path.join(options.commands, `${folder}.txt`), 'utf8')
        result.commands = await runLines(
          linesOf(commands),
          explorer,
          (line) => console.error(`[scene-run]   ${line.ok ? 'ok' : 'FAILED'} ${line.ms}ms ${line.line}`),
          path.join(report, 'captures')
        )
      }
      result.logs = await explorer.sceneLogRead({ since: 0 }).catch((error: unknown) => ({ error: message(error) }))
      try {
        await writeFile(path.join(report, 'final.png'), await explorer.screenshot())
        result.final = 'final.png'
      } catch (error) {
        console.error(`[scene-run]   no final frame: ${message(error)}`)
      }
      await writeFile(path.join(report, 'result.json'), `${JSON.stringify(result, null, 2)}\n`)
      const failed = result.commands.filter((line) => !line.ok).length
      process.stdout.write(
        `${JSON.stringify({ scene: folder, parcel, arrived: result.arrival.ok, commands: result.commands.length, failed, report })}\n`
      )
    }
  } finally {
    await closeSessions()
    await realm.close()
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(PACKAGE_ROOT, 'scripts', 'scene-run.ts')) {
  await main()
}
