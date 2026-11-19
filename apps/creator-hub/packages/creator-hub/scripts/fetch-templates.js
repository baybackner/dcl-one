/**
 * Fetches the scene templates into `scene-templates/`, which the app ships as an extra resource,
 * so a scene made from one needs no network (main/src/modules/download-github-folder.ts).
 *
 * main/src/modules/scene-templates.json pins each template repo to a commit and to the sha256 of
 * GitHub's zip of that commit. A template that is not in it downloads when a scene is created.
 * To move a pin, change its commit and run this script: the mismatch error gives the new sha256.
 *
 * Runs from the electron-builder `beforePack` hook, and on its own with `node`.
 */
import { createHash } from 'crypto';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const packageDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const manifest = path.join(packageDir, 'main', 'src', 'modules', 'scene-templates.json');
const target = path.join(packageDir, 'scene-templates');

const sha256 = data => createHash('sha256').update(data).digest('hex');

export default async function fetchTemplates() {
  const pins = Object.entries(JSON.parse(fs.readFileSync(manifest, 'utf8')));
  fs.mkdirSync(target, { recursive: true });
  for (const [repo, pin] of pins) {
    const file = path.join(target, `${pin.commit}.zip`);
    if (fs.existsSync(file) && sha256(fs.readFileSync(file)) === pin.sha256) continue;
    const url = `${repo.replace('https://github.com/', 'https://codeload.github.com/')}/zip/${pin.commit}`;
    const response = await fetch(url);
    if (!response.ok) throw new Error(`${url} answered ${response.status}`);
    const zip = Buffer.from(await response.arrayBuffer());
    if (sha256(zip) !== pin.sha256) {
      throw new Error(`${url} has sha256 ${sha256(zip)}, the manifest pins ${pin.sha256}`);
    }
    fs.writeFileSync(file, zip);
    console.log(`scene template ${repo} fetched at ${pin.commit.slice(0, 12)}`);
  }
  const kept = new Set(pins.map(([, pin]) => `${pin.commit}.zip`));
  for (const name of fs.readdirSync(target)) {
    if (!kept.has(name)) fs.rmSync(path.join(target, name), { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await fetchTemplates();
}
