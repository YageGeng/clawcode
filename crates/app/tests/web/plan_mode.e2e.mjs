import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { parseArgs } from 'node:util';

import { Page, openBlankPage, startChrome } from './cdp_page.mjs';

// Runs the production app with the repository's claw.toml and its configured
// provider; only the Session data directory and the project are throwaway.
//
//   node crates/app/tests/web/plan_mode.e2e.mjs <keep-planning|approve>
//     [--model provider/model] [--port 3972]
//
// The real model plans, so the script asserts only what a user can observe:
// the approval dialog, the Mode select, the file, and the plan panel.

const { values: options, positionals: [mode] } = parseArgs({
  allowPositionals: true,
  options: {
    model: { type: 'string' },
    port: { type: 'string', default: '3972' }
  }
});
assert(['keep-planning', 'approve'].includes(mode), `unknown mode: ${mode}`);
const base = `http://127.0.0.1:${options.port}/`;
const repo = resolve(import.meta.dirname, '../../../..');
const binary = join(repo, 'target/debug/clawcode');
const COMPOSER = 'textarea[aria-label="消息"]';
const MODE_SELECT = 'select[aria-label="Mode"]';
const MARKER = 'plan-mode-e2e';
const ORIGINAL = 'line one\n';
const MODEL_WAIT_MS = 3 * 60 * 1000;
const PROMPT = `先调研这个仓库，然后调用 exit_plan_mode 提交计划：在 notes.txt 末尾追加一行 ${MARKER}。在 plan 模式下不要尝试修改文件。`;

const work = await mkdtemp(join(tmpdir(), 'clawcode-plan-e2e-'));
const project = join(work, 'project');
const children = [];

/** Creates a small git project whose only file the plan will change. */
async function createProject() {
  mkdirSync(project, { recursive: true });
  await writeFile(join(project, 'notes.txt'), ORIGINAL);
  const git = (...args) => execFileSync('git', args, { cwd: project, stdio: 'ignore' });
  git('init');
  git('add', 'notes.txt');
  git('-c', 'user.name=plan-e2e', '-c', 'user.email=plan-e2e@example.invalid', 'commit', '-m', 'init');
}

/** Uses the repository config, replacing only the active model when asked. */
async function configPath() {
  const source = join(repo, 'claw.toml');
  if (options.model === undefined) return source;
  const config = (await readFile(source, 'utf8'))
    .replace(/^active_model\s*=.*$/m, `active_model = ${JSON.stringify(options.model)}`);
  const path = join(work, 'config.toml');
  await writeFile(path, config);
  return path;
}

/** Waits until the production server answers its bootstrap route. */
async function waitForServer() {
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      const response = await fetch(new URL('/api/ui/bootstrap', base));
      if (response.ok) return;
    } catch { /* The server is still starting. */ }
    await delay(200);
  }
  throw new Error('server did not start');
}

/** Adds the header's Mode select to the shared page driver. */
class PlanPage extends Page {
  /** Chooses a mode the way the header select does. */
  async chooseMode(value) {
    await this.evaluate(`(() => { const select=document.querySelector(${JSON.stringify(MODE_SELECT)}); if(!select)throw Error('missing mode select'); Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(select,${JSON.stringify(value)}); select.dispatchEvent(new Event('change',{bubbles:true})); })()`);
  }
  /** Reads the mode the header shows. */
  mode() {
    return this.evaluate(`document.querySelector(${JSON.stringify(MODE_SELECT)})?.value`);
  }
}

/** Opens the WebUI on a new Session in the project. */
async function openPage() {
  const profile = join(work, 'chrome');
  children.push(startChrome(profile));
  const page = await openBlankPage(profile, PlanPage);
  await page.call('Page.navigate', { url: base });
  await page.wait(`!![...document.querySelectorAll('button')].find(item=>item.textContent.trim()==='新建'&&!item.disabled)`);
  await page.click('新建');
  await page.fill('#session-cwd', project);
  await page.click('创建');
  await page.wait(`!!document.querySelector(${JSON.stringify(COMPOSER)})&&!document.querySelector('#session-cwd')&&!!document.querySelector(${JSON.stringify(MODE_SELECT)})`);
  return page;
}

/** Switches to plan mode, asks for a plan, and waits for the approval dialog. */
async function requestPlan(page) {
  await page.chooseMode('plan');
  await page.wait(`(() => { const select=document.querySelector(${JSON.stringify(MODE_SELECT)}); return select.value==='plan'&&!select.disabled; })()`);
  await page.wait(`!!document.querySelector('.mode-badge')`);
  await page.fill(COMPOSER, PROMPT);
  await page.click('发送');
  await page.wait(`!!document.querySelector('.permission-dialog .permission-plan')?.innerText.trim()`, MODEL_WAIT_MS);
}

/** Waits until no Run is active. */
async function waitIdle(page, timeout = MODEL_WAIT_MS) {
  await page.wait(`!document.querySelector('.danger-button')`, timeout);
}

/** Keeping the plan leaves plan mode on, the file untouched, and the composer usable. */
async function keepPlanningMode(page) {
  await requestPlan(page);
  await page.click('继续规划');
  await page.wait(`!document.querySelector('.permission-dialog')`);
  await waitIdle(page, 60_000);
  assert.equal(await page.mode(), 'plan', 'the Session stays in plan mode');
  assert.equal(await readFile(join(project, 'notes.txt'), 'utf8'), ORIGINAL, 'plan mode changed nothing');
  assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(COMPOSER)}).disabled`), false, 'the composer takes feedback');
}

/** Approval returns to default mode, the plan runs, and the plan panel survives a reload. */
async function approveMode(page) {
  await requestPlan(page);
  await page.click('批准并执行');
  await page.wait(`document.querySelector(${JSON.stringify(MODE_SELECT)})?.value==='default'`, 60_000);
  const until = Date.now() + MODEL_WAIT_MS;
  while (!(await readFile(join(project, 'notes.txt'), 'utf8')).includes(MARKER)) {
    assert(Date.now() < until, 'the approved plan edited notes.txt');
    await delay(500);
  }
  await waitIdle(page);
  await page.wait(`!!document.querySelector('.plan-panel__approved')`);
  if (await page.evaluate(`!!document.querySelector('.plan-panel__todos')`)) {
    assert.match(await page.evaluate(`document.querySelector('.plan-panel__progress')?.textContent ?? ''`), /^\d+\/\d+$/);
  }
  // A full replay rebuilds the panel from the Session's history.
  await page.call('Page.reload');
  await page.wait(`!!document.querySelector('.plan-panel__approved')`, 60_000);
}

let failed = false;
try {
  await createProject();
  const config = await configPath();
  children.push(spawn(binary, ['serve', '--bind', `127.0.0.1:${options.port}`, '--web-root', join(repo, 'web/dist')], {
    cwd: repo,
    env: { ...process.env, CLAW_CONFIG: config, CLAW_SESSION_PERSISTENCE__DATA_HOME: join(work, 'data') },
    stdio: 'ignore'
  }));
  await waitForServer();
  const page = await openPage();
  if (mode === 'keep-planning') await keepPlanningMode(page);
  else await approveMode(page);
  assert.deepEqual(page.errors, [], 'the page threw no exceptions');
  console.log(`plan mode e2e passed: ${mode}`);
} catch (error) {
  failed = true;
  console.error(`plan mode e2e failed: ${mode}\n${error.stack ?? error}`);
} finally {
  for (const child of children) child.kill();
  await delay(300);
  await rm(work, { recursive: true, force: true, maxRetries: 3 });
}
process.exit(failed ? 1 : 0);
