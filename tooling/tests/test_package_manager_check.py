"""Internal JavaScript projects install with pnpm."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('package_manager_check', Path(__file__).parents[1] / 'package-manager-check.py')
check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check)


def write(root, rel, text=''):
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding='utf-8')


class PackageManagerTest(unittest.TestCase):
    def test_pnpm_projects_publishing_and_creator_scenes_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write(root, 'apps/x/pnpm-lock.yaml')
            write(root, 'apps/x/package.json', '{"name": "x"}')
            write(root, 'apps/x/pnpm-workspace.yaml', 'ignoreScripts: true\nsaveExact: true\n')
            write(root, 'infra/dev/service-test/fixture/apps/x/pnpm-lock.yaml')
            write(root, 'infra/dev/service-test/fixture/apps/x/package.json', '{"packageManager": "pnpm@10.19.0"}')
            write(root, 'apps/x/.npmrc', '\n'.join((
                '; registry and auth only',
                'registry=https://registry.npmjs.org/',
                '@dcl:registry=https://registry.npmjs.org/',
                '//registry.npmjs.org/:_authToken=${NPM_TOKEN}',
                'strict-ssl = true')))
            write(root, 'apps/x/node_modules/y/package-lock.json')
            write(root, 'library/js-sdk-toolchain/test/snapshots/package-lock.json')
            write(root, '.github/actions/setup-pnpm/action.yml',
                  '# Not corepack: Node 25+ ships without it.\n    env:\n      PNPM_VERSION: 12.21.0\n'
                  '    env:\n      PNPM_VERSION: 12.21.0\n')
            write(root, '.github/actions/setup-pnpm/digests.txt', '# pnpm 12.21.0\n')
            write(root, '.github/workflows/x.yml', '\n'.join((
                '      # npm run build and corepack enable are what this used to call',
                "      paths: ['**/yarn.lock', '**/package-lock.json']",
                '      - uses: ./.github/actions/setup-pnpm',
                '      - run: pnpm install --frozen-lockfile && pnpm run build && pnpm dlx skills@1.7.0 add x',
                '      - run: npm pack && npm publish "$tgz" --access public && npm whoami',
                '      - run: npm --registry https://registry.npmjs.org view "$pkg" version && npm pkg set x=1',
                '      - run: npm view "$pkg" dist-tags --json && npx --yes npm@11.21.1 dist-tag rm "$pkg" "$TAG"')))
            self.assertEqual(check.offenders(root), [])

    def test_npm_lockfiles_pins_unread_settings_and_other_pnpm_fetches_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write(root, 'apps/x/package-lock.json')
            write(root, 'apps/y/yarn.lock')
            write(root, 'apps/p/pnpm-lock.yaml')
            write(root, 'apps/p/package.json', '{"packageManager": "pnpm@12.21.0+sha512.abc"}')
            write(root, 'apps/z/.npmrc', '\n'.join((
                'ignore-scripts=true', 'save-exact = true', 'allow-builds=esbuild', 'minimum-release-age=0', 'fund=false')))
            commands = (
                'npm ci', 'cd app && npm run build', 'npx tsc', 'npm --prefix app ci', 'npm -w pkg run build',
                'npm --silent install', 'npx --yes npm@11.21.1 ci', 'yarn install', 'cd app && yarnpkg add thing')
            fetches = ('corepack enable', 'corepack pnpm@12.21.0 install', 'pnpm add --global pnpm@12.21.0')
            write(root, '.github/workflows/x.yml', ''.join(f'      - run: {command}\n' for command in (*commands, *fetches)))
            write(root, '.github/actions/a/run.sh', 'npm install --global thing\n')
            write(root, '.github/actions/setup-pnpm/action.yml', '      PNPM_VERSION: 12.21.0\n      PNPM_VERSION: 12.18.0\n')
            write(root, '.github/actions/setup-pnpm/digests.txt', '# pnpm 12.21.0\n')
            self.assertEqual(check.offenders(root), [
                '.github/actions/a/run.sh:1: installs or runs with npm or yarn; use pnpm',
                *(f'.github/workflows/x.yml:{line}: installs or runs with npm or yarn; use pnpm'
                  for line in range(1, len(commands) + 1)),
                *(f'.github/workflows/x.yml:{line}: fetches pnpm itself; use .github/actions/setup-pnpm'
                  for line in range(len(commands) + 1, len(commands) + len(fetches) + 1)),
                'apps/p/package.json: pins `pnpm@12.21.0+sha512.abc`; remove packageManager, CI takes pnpm from '
                '.github/actions/setup-pnpm',
                'apps/x/package-lock.json: npm or yarn lockfile; convert it with `pnpm import` and delete it',
                'apps/y/yarn.lock: npm or yarn lockfile; convert it with `pnpm import` and delete it',
                *(f'apps/z/.npmrc: pnpm does not read `{key}` here; move it to pnpm-workspace.yaml or delete it'
                  for key in ('ignore-scripts', 'save-exact', 'allow-builds', 'minimum-release-age', 'fund')),
                '.github/actions/setup-pnpm: action.yml names pnpm 12.18.0, 12.21.0 and digests.txt 12.21.0; '
                'both name one release'])

    def test_the_repository_passes(self):
        self.assertEqual(check.offenders(check.ROOT), [])


if __name__ == '__main__':
    unittest.main()
