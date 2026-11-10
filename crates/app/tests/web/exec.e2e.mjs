import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { mkdirSync, openSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { parseArgs } from 'node:util';

import { Page, openBlankPage, startChrome } from './cdp_page.mjs';

// Runs the production app with the repository's claw.toml and its configured
// provider; only the Session data directory and the project are throwaway.
//
//   node crates/app/tests/web/exec.e2e.mjs <card|badge>
//     [--model provider/model] [--port 3973]
//
// The real model runs the commands, so the script asserts only what a user
// can observe: tool cards, their terminals, and the background-terminal list.

const { values: options, positionals: [scenario] } = parseArgs({
  allowPositionals: true,
  options: {
    model: { type: 'string' },
    port: { type: 'string', default: '3973' }
  }
});
assert(['card', 'badge'].includes(scenario), `unknown scenario: ${scenario}`);
const base = `http://127.0.0.1:${options.port}/`;
const repo = resolve(import.meta.dirname, '../../../..');
const binary = join(repo, 'target/debug/clawcode');
const COMPOSER = 'textarea[aria-label="消息"]';
const MODEL_WAIT_MS = 3 * 60 * 1000;
const EXEC_CARD = '.tool-card[data-kind="execute"]';

const work = await mkdtemp(join(tmpdir(), 'clawcode-exec-e2e-'));
const project = join(work, 'project');
const children = [];

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

/** Reads exec tool cards the way a user sees them. */
class ExecPage extends Page {
  /** Status and terminal text of every exec_command card, in transcript order. */
  cards() {
    return this.evaluate(`[...document.querySelectorAll(${JSON.stringify(EXEC_CARD)})]
      .filter((card) => card.querySelector('.tool-card__title')?.textContent.trim() === 'exec_command')
      .map((card) => ({
        status: card.dataset.status,
        open: card.open,
        output: card.querySelector('.terminal-view__output')?.textContent ?? '',
        exit: card.querySelector('.terminal-view__exit')?.textContent ?? ''
      }))`);
  }
  /** Opens the first collapsed exec_command card. */
  async expandFirstCard() {
    await this.evaluate(`(() => {
      const card = [...document.querySelectorAll(${JSON.stringify(EXEC_CARD)})]
        .find((item) => item.querySelector('.tool-card__title')?.textContent.trim() === 'exec_command' && !item.open);
      card?.querySelector('summary')?.click();
    })()`);
  }
}

/** Opens the WebUI on a new Session in the project. */
async function openPage() {
  const profile = join(work, 'chrome');
  children.push(startChrome(profile));
  const page = await openBlankPage(profile, ExecPage);
  await page.call('Page.navigate', { url: base });
  await page.wait(`!![...document.querySelectorAll('button')].find(item=>item.textContent.trim()==='新建'&&!item.disabled)`);
  await page.click('新建');
  await page.fill('#session-cwd', project);
  await page.click('创建');
  await page.wait(`!!document.querySelector(${JSON.stringify(COMPOSER)})&&!document.querySelector('#session-cwd')`);
  return page;
}

/** Sends one prompt from the composer. */
async function send(page, prompt) {
  await page.fill(COMPOSER, prompt);
  await page.click('发送');
}

/** Waits until no Run is active. */
async function waitIdle(page, timeout = MODEL_WAIT_MS) {
  await page.wait(`!document.querySelector('.danger-button')`, timeout);
}

/**
 * A command that outlives its turn keeps streaming into its card after the
 * Run ends, and the card settles with the exit code when the process exits.
 */
async function cardScenario(page) {
  await send(page, '调用一次 exec_command 运行 `for i in 1 2 3 4 5; do echo tick-$i; sleep 2; done`，yield_time_ms 设为 1000，login 设为 false。工具返回 session ID 后不要轮询、不要再调用任何工具，直接用一句话结束回复。');
  await page.wait(`document.querySelectorAll(${JSON.stringify(EXEC_CARD)}).length > 0`, MODEL_WAIT_MS);
  await waitIdle(page);
  const running = await page.cards();
  assert.equal(running.length, 1, 'one exec_command card');
  assert.equal(running[0].status, 'in_progress', 'the card is still running after the Run ended');
  assert(!running[0].output.includes('tick-5'), 'the command had not finished when the Run ended');
  await page.wait(`[...document.querySelectorAll(${JSON.stringify(EXEC_CARD)})].some((card) => card.dataset.status === 'completed')`, 20_000);
  await page.expandFirstCard();
  await page.wait(`[...document.querySelectorAll('.terminal-view__output')].some((output) => output.textContent.includes('tick-5'))`, 5_000);
  const [settled] = await page.cards();
  assert.match(settled.exit, /退出码 0/, 'the card shows the exit code');
}

/**
 * The background-terminal list counts running commands, stops one, then all;
 * every stopped command's card ends failed with the signal that killed it.
 */
async function badgeScenario(page) {
  await send(page, '调用两次 exec_command，每次都运行 `sleep 300`，yield_time_ms 设为 1000，login 设为 false。两次调用都返回 session ID 后不要轮询、不要再调用任何工具，直接用一句话结束回复。');
  const count = `document.querySelector('.background-terminals__count')?.textContent.trim() ?? ''`;
  // The count appears once the Run started, so idle after it means both commands returned.
  await page.wait(`${count} === '2 个后台终端'`, MODEL_WAIT_MS);
  await waitIdle(page);
  await page.evaluate(`document.querySelector('.background-terminals__toggle').click()`);
  await page.wait(`document.querySelectorAll('.background-terminals__item').length === 2`, 10_000);
  await page.evaluate(`document.querySelector('.background-terminals__item button').click()`);
  await page.wait(`${count} === '1 个后台终端'`, 10_000);
  // The previous action still reloads the list; its buttons are disabled until then.
  await page.wait(`document.querySelector('.background-terminals__stop-all')?.disabled === false`, 10_000);
  await page.evaluate(`document.querySelector('.background-terminals__stop-all').click()`);
  await page.wait(`!document.querySelector('.background-terminals')`, 10_000);
  await page.wait(`[...document.querySelectorAll(${JSON.stringify(EXEC_CARD)})].filter((card) => card.dataset.status === 'failed').length === 2`, 10_000);
  await page.expandFirstCard();
  await page.wait(`[...document.querySelectorAll('.terminal-view__exit')].some((exit) => exit.textContent.includes('信号 SIGKILL'))`, 5_000);
}

let failed = false;
try {
  mkdirSync(project, { recursive: true });
  const config = await configPath();
  children.push(spawn(binary, ['serve', '--bind', `127.0.0.1:${options.port}`, '--web-root', join(repo, 'web/dist')], {
    cwd: repo,
    env: { ...process.env, CLAW_CONFIG: config, CLAW_SESSION_PERSISTENCE__DATA_HOME: join(work, 'data') },
    stdio: process.env.EXEC_E2E_LOG === undefined ? 'ignore' : ['ignore', openSync(process.env.EXEC_E2E_LOG, 'w'), openSync(process.env.EXEC_E2E_LOG + '.err', 'w')]
  }));
  await waitForServer();
  const page = await openPage();
  if (scenario === 'card') await cardScenario(page);
  else await badgeScenario(page);
  assert.deepEqual(page.errors, [], 'the page threw no exceptions');
  console.log(`exec e2e passed: ${scenario}`);
} catch (error) {
  failed = true;
  console.error(`exec e2e failed: ${scenario}\n${error.stack ?? error}`);
} finally {
  for (const child of children) child.kill();
  await delay(300);
  await rm(work, { recursive: true, force: true, maxRetries: 3 });
}
process.exit(failed ? 1 : 0);
