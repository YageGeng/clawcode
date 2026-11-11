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
//   node crates/app/tests/web/subagent.e2e.mjs [--model provider/model] [--port 3974]
//
// The real model drives the agent tools, so the script asserts only what a
// user can observe: result mail cards, nickname pills, and spawn cards whose
// compact child view collapses the inherited history.

const { values: options } = parseArgs({
  options: {
    model: { type: 'string' },
    port: { type: 'string', default: '3974' }
  }
});
const base = `http://127.0.0.1:${options.port}/`;
const repo = resolve(import.meta.dirname, '../../../..');
const binary = join(repo, 'target/debug/clawcode');
const COMPOSER = 'textarea[aria-label="消息"]';
const MODEL_WAIT_MS = 4 * 60 * 1000;
const RESULT_CARD = '.agent-mail-card[data-kind="result"]';

const work = await mkdtemp(join(tmpdir(), 'clawcode-subagent-e2e-'));
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

/** Opens the WebUI on a new Session in the project. */
async function openPage() {
  const profile = join(work, 'chrome');
  children.push(startChrome(profile));
  const page = await openBlankPage(profile, Page);
  await page.call('Page.navigate', { url: base });
  await page.wait(`!![...document.querySelectorAll('button')].find(item=>item.textContent.trim()==='新建'&&!item.disabled)`);
  await page.click('新建');
  await page.fill('#session-cwd', project);
  await page.click('创建');
  await page.wait(`!!document.querySelector(${JSON.stringify(COMPOSER)})&&!document.querySelector('#session-cwd')`);
  return page;
}

/**
 * Two spawned children answer; their results arrive as mail cards, the pill
 * strip shows their nicknames, and a spawn card's compact child view starts at
 * the child's task with one line for the inherited history.
 */
async function scenario(page) {
  await page.fill(COMPOSER, '调用两次 spawn_agent：task_name 分别为 alpha 和 beta，message 分别为"用一句话说出一个质数"和"用一句话说出一种颜色"，不要设置 fork_turns。两次 spawn 都返回后，反复调用 wait_agent，直到你的上下文里出现 alpha 和 beta 两条 result 消息，然后用一句话总结并结束，不要调用其他工具。');
  await page.click('发送');
  await page.wait(`document.querySelectorAll(${JSON.stringify(RESULT_CARD)}).length >= 2`, MODEL_WAIT_MS);
  await page.wait(`!document.querySelector('.danger-button')`, MODEL_WAIT_MS);

  // The collapsed mail summary keeps the routing label visible without its body.
  const senders = await page.evaluate(`[...document.querySelectorAll(${JSON.stringify(RESULT_CARD)})].map((card) => card.querySelector('.agent-mail-card__route')?.textContent ?? '')`);
  assert(senders.some((text) => text.includes('/root/alpha')), `alpha result card: ${senders}`);
  assert(senders.some((text) => text.includes('/root/beta')), `beta result card: ${senders}`);

  await page.wait(`document.querySelectorAll('.subagent-pill').length === 2`, 15_000);
  const pills = await page.evaluate(`[...document.querySelectorAll('.subagent-pill__open')].map((pill) => ({ title: pill.title, name: pill.querySelector('.subagent-pill__name')?.textContent ?? '' }))`);
  assert.deepEqual(pills.map((pill) => pill.title).sort(), ['/root/alpha', '/root/beta']);
  assert(pills.every((pill) => pill.name.length > 0 && !pill.name.startsWith('/root')), `nickname pills: ${JSON.stringify(pills)}`);

  // Finished children collapse their spawn cards; opening one shows the compact view.
  const spawnCard = `[...document.querySelectorAll('.tool-card')].find((card) => card.querySelector('.tool-card__title')?.textContent.trim() === 'spawn_agent')`;
  assert.equal(await page.evaluate(`${spawnCard}?.open`), false, 'spawn card is collapsed once its child settled');
  await page.evaluate(`${spawnCard}.querySelector('summary').click()`);
  await page.wait(`!!${spawnCard}?.querySelector('.subagent-panel .agent-mail-card[data-kind="new_task"]')`, 15_000);
  // Reopening first paints the cached live transcript, then the full replay
  // lands, so wait for the collapsed line instead of reading one frame.
  await page.wait(`/继承了 \\d+ 条父会话历史/.test(${spawnCard}.querySelector('.subagent-panel .inherited-history')?.textContent ?? '')`, 15_000);
}

let failed = false;
let page;
try {
  mkdirSync(project, { recursive: true });
  const config = await configPath();
  children.push(spawn(binary, ['serve', '--bind', `127.0.0.1:${options.port}`, '--web-root', join(repo, 'web/dist')], {
    cwd: repo,
    env: { ...process.env, CLAW_CONFIG: config, CLAW_SESSION_PERSISTENCE__DATA_HOME: join(work, 'data') },
    // The server log stays with the work directory, which a failure keeps.
    stdio: ['ignore', openSync(join(work, 'server.log'), 'w'), openSync(join(work, 'server.log'), 'a')]
  }));
  await waitForServer();
  page = await openPage();
  await scenario(page);
  assert.deepEqual(page.errors, [], 'the page threw no exceptions');
  console.log('subagent e2e passed');
} catch (error) {
  failed = true;
  console.error(`subagent e2e failed\n${error.stack ?? error}`);
  // A real model makes failures hard to replay; keep what the user would have seen.
  const shot = await page?.call('Page.captureScreenshot', { format: 'png' }).catch(() => undefined);
  if (shot !== undefined) await writeFile(join(work, 'failure.png'), Buffer.from(shot.data, 'base64'));
  console.error(`kept server.log, failure.png, and Session data in ${work}`);
} finally {
  for (const child of children) child.kill();
  await delay(300);
  if (!failed) await rm(work, { recursive: true, force: true, maxRetries: 3 });
}
process.exit(failed ? 1 : 0);
