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
//   node crates/app/tests/web/ask_user.e2e.mjs <answer|skip|supersede>
//     [--model provider/model] [--port 3973]
//
// The real model asks, so the script asserts only what a user can observe:
// the question panel, the question card's status, and the reply records.

const { values: options, positionals: [mode] } = parseArgs({
  allowPositionals: true,
  options: {
    model: { type: 'string' },
    port: { type: 'string', default: '3973' }
  }
});
assert(['answer', 'skip', 'supersede'].includes(mode), `unknown mode: ${mode}`);
const base = `http://127.0.0.1:${options.port}/`;
const repo = resolve(import.meta.dirname, '../../../..');
const binary = join(repo, 'target/debug/clawcode');
const COMPOSER = 'textarea[aria-label="消息"]';
const PANEL = '.question-panel';
const MODEL_WAIT_MS = 3 * 60 * 1000;
const PROMPT = '调用 request_user_input_async 询问我偏好的数据库，选项 sqlite 和 postgres，然后在等待期间继续回复"继续工作"。';

const work = await mkdtemp(join(tmpdir(), 'clawcode-ask-e2e-'));
const project = join(work, 'project');
const children = [];

/** Creates an empty git project for the Session. */
async function createProject() {
  mkdirSync(project, { recursive: true });
  await writeFile(join(project, 'README.md'), 'ask e2e\n');
  const git = (...args) => execFileSync('git', args, { cwd: project, stdio: 'ignore' });
  git('init');
  git('add', 'README.md');
  git('-c', 'user.name=ask-e2e', '-c', 'user.email=ask-e2e@example.invalid', 'commit', '-m', 'init');
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

/** Adds question-panel interactions to the shared page driver. */
class QuestionPage extends Page {
  /** Clicks the enabled button labelled `text` inside `container`; the panel and composer both have 发送. */
  async clickIn(container, text) {
    await this.evaluate(`(() => { const button=[...document.querySelectorAll(${JSON.stringify(`${container} button`)})].find(item=>item.textContent.trim()===${JSON.stringify(text)}&&!item.disabled); if(!button)throw Error('missing button '+${JSON.stringify(text)}); button.click(); })()`);
  }
  /** Selects the panel suggestion labelled `text`. */
  async choose(text) {
    await this.evaluate(`(() => { const label=[...document.querySelectorAll(${JSON.stringify(`${PANEL} label`)})].find(item=>item.textContent.trim()===${JSON.stringify(text)}); if(!label)throw Error('missing option '+${JSON.stringify(text)}); label.querySelector('input').click(); })()`);
  }
  /** Reads the question card's status line. */
  questionStatus() {
    return this.evaluate(`document.querySelector('.asked-questions__status')?.textContent ?? ''`);
  }
  /** Opens the question card so its status list renders. */
  async openQuestionCard() {
    await this.evaluate(`(() => { const card=[...document.querySelectorAll('.tool-card')].find(item=>item.querySelector('.tool-card__title')?.textContent==='request_user_input_async'); if(!card)throw Error('missing question card'); if(!card.open)card.querySelector('summary').click(); })()`);
    await this.wait(`!!document.querySelector('.asked-questions')`);
  }
}

/** Opens the WebUI on a new Session in the project. */
async function openPage() {
  const profile = join(work, 'chrome');
  children.push(startChrome(profile));
  const page = await openBlankPage(profile, QuestionPage);
  await page.call('Page.navigate', { url: base });
  await page.wait(`!![...document.querySelectorAll('button')].find(item=>item.textContent.trim()==='新建'&&!item.disabled)`);
  await page.click('新建');
  await page.fill('#session-cwd', project);
  await page.click('创建');
  await page.wait(`!!document.querySelector(${JSON.stringify(COMPOSER)})&&!document.querySelector('#session-cwd')`);
  return page;
}

/** Waits until no Run is active. */
async function waitIdle(page, timeout = MODEL_WAIT_MS) {
  await page.wait(`!document.querySelector('.danger-button')`, timeout);
}

/** Asks for the question and waits for the panel, which must not take focus. */
async function askQuestion(page) {
  await page.fill(COMPOSER, PROMPT);
  await page.clickIn('.composer-card', '发送');
  await page.wait(`!!document.querySelector(${JSON.stringify(PANEL)})`, MODEL_WAIT_MS);
  assert.equal(
    await page.evaluate(`!document.querySelector(${JSON.stringify(PANEL)}).contains(document.activeElement)`),
    true,
    'an arriving question does not take focus'
  );
}

/** Answering records the choice, settles the card, and closes the panel. */
async function answerMode(page) {
  await askQuestion(page);
  await page.choose('postgres');
  await page.clickIn(PANEL, '发送');
  await page.wait(`!document.querySelector(${JSON.stringify(PANEL)})`, 60_000);
  await page.wait(`[...document.querySelectorAll('.question-reply')].some(item=>item.textContent.includes('postgres'))`, 60_000);
  await page.openQuestionCard();
  assert.equal(await page.questionStatus(), '已回答：postgres');
}

/** A skip survives a reload: the card says skipped and the panel stays closed. */
async function skipMode(page) {
  await askQuestion(page);
  await page.clickIn(PANEL, '跳过');
  await page.wait(`!document.querySelector(${JSON.stringify(PANEL)})`, 60_000);
  await waitIdle(page);
  await page.call('Page.reload');
  await page.wait(`!!document.querySelector('.question-reply--skipped')`, 60_000);
  await page.openQuestionCard();
  assert.equal(await page.questionStatus(), '已跳过');
  assert.equal(await page.evaluate(`!!document.querySelector(${JSON.stringify(PANEL)})`), false, 'the skipped question stays closed');
}

/** A normal message skips the open question; the kernel records the skip with the message's Run. */
async function supersedeMode(page) {
  await askQuestion(page);
  await waitIdle(page);
  await page.fill(COMPOSER, '换个话题');
  await page.clickIn('.composer-card', '发送');
  await page.wait(`!document.querySelector(${JSON.stringify(PANEL)})`, 60_000);
  await page.wait(`!!document.querySelector('.question-reply--skipped')`, 60_000);
  assert.equal(
    await page.evaluate(`[...document.querySelectorAll('.message[data-role="user"]')].some(item=>item.textContent.includes('换个话题'))`),
    true,
    'the new message was sent'
  );
  assert.equal(await page.evaluate(`document.querySelector('.question-reply--skipped')?.textContent.trim()`), '已跳过 1 个问题');
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
  if (mode === 'answer') await answerMode(page);
  else if (mode === 'skip') await skipMode(page);
  else await supersedeMode(page);
  assert.deepEqual(page.errors, [], 'the page threw no exceptions');
  console.log(`ask user e2e passed: ${mode}`);
} catch (error) {
  failed = true;
  console.error(`ask user e2e failed: ${mode}\n${error.stack ?? error}`);
} finally {
  for (const child of children) child.kill();
  await delay(300);
  await rm(work, { recursive: true, force: true, maxRetries: 3 });
}
process.exit(failed ? 1 : 0);
