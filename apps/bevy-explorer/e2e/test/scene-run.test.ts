import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { describe, test } from 'node:test'
import { REPORT_DIR } from '../driver/paths.ts'
import { findScenes, parseArgs, selectRuns } from '../scripts/scene-run.ts'

describe('scene-run arguments', () => {
  test('takes the root, the command folder and the options, resolved from the working folder', () => {
    const cwd = path.join(path.sep, 'work')
    const options = parseArgs(['--scenes', 'ws/scenes', '--commands', 'cmds', '--scene', '0,1-a', '--scene', '2,2-b', '--report', 'out', '--web-app', 'web'], cwd)
    assert.deepEqual(options, {
      root: path.join(cwd, 'ws', 'scenes'),
      commands: path.join(cwd, 'cmds'),
      only: ['0,1-a', '2,2-b'],
      report: path.join(cwd, 'out'),
      webApp: path.join(cwd, 'web')
    })
  })

  test('reports into report/scene-run by default and skips a separator', () => {
    const options = parseArgs(['--', '--scenes', 'a', '--commands', 'b'])
    assert.equal(options.report, path.join(REPORT_DIR, 'scene-run'))
    assert.deepEqual(options.only, [])
  })

  test('refuses a run without a root or a command folder, a missing value and an unknown flag', () => {
    assert.throws(() => parseArgs(['--commands', 'b']), /--scenes <root> is needed/)
    assert.throws(() => parseArgs(['--scenes', 'a']), /--commands <dir> is needed/)
    assert.throws(() => parseArgs(['--scenes', '--commands', 'b']), /--scenes needs a value/)
    assert.throws(() => parseArgs(['--scenes', 'a', '--commands', 'b', '--position', '1,1']), /unknown argument --position/)
  })
})

describe('scene-run discovery', () => {
  test('lists folders with scene.json, built when the main bundle is there', async () => {
    const root = await mkdtemp(path.join(tmpdir(), 'scene-run-'))
    try {
      const scene = async (folder: string, metadata: object, bundle?: string) => {
        await mkdir(path.join(root, folder, 'bin'), { recursive: true })
        await writeFile(path.join(root, folder, 'scene.json'), JSON.stringify(metadata))
        if (bundle) await writeFile(path.join(root, folder, bundle), '')
      }
      await scene('1,0-b', { scene: { base: '1,0' } }, 'bin/index.js')
      await scene('0,0-a', { scene: { base: '0,0' }, main: 'bin/game.js' }, 'bin/game.js')
      await scene('2,0-c', { scene: { base: '2,0' } })
      await mkdir(path.join(root, 'not-a-scene'))
      await writeFile(path.join(root, 'README.md'), '')
      assert.deepEqual(await findScenes(root), { built: ['0,0-a', '1,0-b'], unbuilt: ['2,0-c'] })
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  })

  test('runs the built folders with a command file, narrowed to the ones asked for', () => {
    const built = ['0,0-a', '1,0-b', '2,0-c']
    const files = ['1,0-b.txt', '2,0-c.txt', 'notes.md', '9,9-z.txt']
    assert.deepEqual(selectRuns(built, files, []), { runs: ['1,0-b', '2,0-c'], missing: [] })
    assert.deepEqual(selectRuns(built, files, ['2,0-c', '0,0-a', '9,9-z']), {
      runs: ['2,0-c'],
      missing: ['0,0-a: no 0,0-a.txt', '9,9-z: not a built scene folder']
    })
  })
})
