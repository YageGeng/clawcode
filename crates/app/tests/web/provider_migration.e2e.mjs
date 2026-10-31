import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

// Run against the production app and its configured provider; no model or backend is substituted.
const base = new URL(process.argv[2] ?? 'http://127.0.0.1:3001');
const work = resolve(process.argv[3] ?? '/tmp/clawcode-rig-core-e2e/work');
const evidence = resolve(process.argv[4] ?? '/tmp/clawcode-rig-core-e2e/evidence');
assert(['127.0.0.1', 'localhost'].includes(base.hostname), 'Use an isolated local production app');
await mkdir(evidence, { recursive: true });
const profile = await mkdtemp(join(evidence, 'chrome-'));
const browser = spawn(process.env.CHROME_BIN ?? '/opt/google/chrome/chrome', [
  '--headless=new', '--no-sandbox', '--disable-gpu', '--no-first-run', '--disable-dev-shm-usage',
  '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'
], { stdio: ['ignore', 'ignore', 'ignore'] });

/** Owns one CDP connection and correlates only this script's requests. */
class Page {
  pending = new Map();
  errors = [];
  sequence = 0;
  constructor(socket) {
    this.socket = socket;
    socket.addEventListener('message', ({ data }) => {
      const frame = JSON.parse(data);
      if (frame.method === 'Runtime.exceptionThrown') this.errors.push(frame.params.exceptionDetails.text);
      const waiter = this.pending.get(frame.id);
      if (!waiter) return;
      this.pending.delete(frame.id);
      if (frame.error) waiter.reject(new Error(JSON.stringify(frame.error)));
      else waiter.resolve(frame.result);
    });
  }
  /** Sends a CDP operation without relying on an additional browser framework. */
  call(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = ++this.sequence;
      this.pending.set(id, { resolve, reject });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }
  /** Evaluates an observable page action and propagates page exceptions. */
  async evaluate(expression) {
    const result = await this.call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
  }
  /** Waits for a visible or protocol condition within a bounded acceptance window. */
  async wait(expression, timeout = 180000) {
    const until = Date.now() + timeout;
    while (Date.now() < until) {
      const value = await this.evaluate(expression);
      if (value) return value;
      await delay(100);
    }
    throw new Error(`Timed out: ${expression}`);
  }
  /** Uses the native value setter so the real React input handler receives the edit. */
  async fill(selector, value) {
    await this.evaluate(`(() => { const element=document.querySelector(${JSON.stringify(selector)}); if(!element)throw Error('missing input'); const prototype=element.tagName==='TEXTAREA'?HTMLTextAreaElement.prototype:HTMLInputElement.prototype; Object.getOwnPropertyDescriptor(prototype,'value').set.call(element,${JSON.stringify(value)}); element.dispatchEvent(new Event('input',{bubbles:true})); })()`);
  }
  /** Clicks a visible enabled button by its exact displayed text. */
  async click(text) {
    await this.evaluate(`(() => {const button=[...document.querySelectorAll('button')].find(item=>item.textContent.trim()===${JSON.stringify(text)}&&!item.disabled);if(!button)throw Error('missing button '+${JSON.stringify(text)});button.click()})()`);
  }
  /** Submits through the real Composer and waits until a request is actually sent. */
  async send(text, button = '发送') {
    const previous = await this.evaluate('window.__audit.sent.length');
    await this.fill('textarea[aria-label="消息"]', text);
    await this.click(button);
    await this.wait(`window.__audit.sent.length > ${previous}`);
  }
  /** Calls an existing production ACP method on the page's authenticated local socket. */
  rpc(method, params) {
    return this.evaluate(`window.__audit.rpc(${JSON.stringify(method)},${JSON.stringify(params)})`);
  }
  /** Waits for the authoritative final assistant snapshot and idle state of one session. */
  async answer(sessionId, marker) {
    await this.wait(`window.__audit.received.flat().some(frame=>frame.method==='session/update'&&frame.params.sessionId===${JSON.stringify(sessionId)}&&frame.params.update.sessionUpdate==='agent_message'&&JSON.stringify(frame.params.update.content).includes(${JSON.stringify(marker)}))`);
    await this.wait(`window.__audit.received.flat().filter(frame=>frame.method==='session/update'&&frame.params.sessionId===${JSON.stringify(sessionId)}&&frame.params.update.sessionUpdate==='state_update').at(-1)?.params.update.state==='idle'`);
  }
  /** Captures the actual rendered production page for later review. */
  async screenshot(name) {
    const result = await this.call('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true });
    await writeFile(join(evidence, `${name}.png`), Buffer.from(result.data, 'base64'));
  }
}

let page;
const results = [];
try {
  let port;
  for (let attempt = 0; attempt < 100; attempt++) {
    try { port = Number((await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]); break; }
    catch { await delay(100); }
  }
  assert(port, 'Chrome started');
  const target = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: 'PUT' })).json();
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, { once: true }); socket.addEventListener('error', reject, { once: true }); });
  page = new Page(socket);
  await page.call('Runtime.enable');
  await page.call('Page.enable');
  await page.call('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1100, deviceScaleFactor: 1, mobile: false });
  // Observe genuine ACP traffic and DOM snapshots; never fabricate provider replies.
  await page.call('Page.addScriptToEvaluateOnNewDocument', { source: `
    window.__audit={sent:[],received:[],samples:[],socket:null,rpcId:0};
    const NativeWebSocket=window.WebSocket;
    window.WebSocket=class extends NativeWebSocket {
      constructor(...args){super(...args);window.__audit.socket=this;this.addEventListener('message',event=>{try{window.__audit.received.push(JSON.parse(event.data))}catch{}})}
      send(value){try{window.__audit.sent.push(JSON.parse(value))}catch{}return super.send(value)}
    };
    window.__audit.rpc=(method,params)=>new Promise((resolve,reject)=>{const socket=window.__audit.socket;const id='audit-'+(++window.__audit.rpcId);const listener=event=>{const frame=JSON.parse(event.data);if(frame.id!==id)return;socket.removeEventListener('message',listener);frame.error?reject(Error(JSON.stringify(frame.error))):resolve(frame.result)};socket.addEventListener('message',listener);socket.send(JSON.stringify({jsonrpc:'2.0',id,method,params}))});
    setInterval(()=>{if(!document.querySelector('.danger-button'))return;const visible=[...document.querySelectorAll('.message[data-role=assistant] .message__body,.reasoning-block,.tool-card')].map(node=>node.innerText).join(' ');if(visible)window.__audit.samples.push({time:Date.now(),text:visible.slice(-3000)})},100);
  ` });
  await page.call('Page.navigate', { url: base.href });
  const bootstrap = await (await fetch(new URL('/api/ui/bootstrap', base))).json();
  results.push({ scenario: 'production-bootstrap', model: bootstrap.activeModel.id });
  await page.wait(`!![...document.querySelectorAll('button')].find(item=>item.textContent.trim()==='新建'&&!item.disabled)`, 20000);
  await page.click('新建');
  await page.fill('#session-cwd', work);
  await page.click('创建');
  await page.wait(`!!document.querySelector('textarea[aria-label="消息"]')&&!document.querySelector('#session-cwd')`, 20000);
  await page.send('这是隔离目录的迁移验收。请先单独用 exec_command 执行 printf RIG_MIGRATION_ONE，收到结果后再单独执行 printf RIG_MIGRATION_TWO。不要使用子代理，不读取或写入文件。完成后只回答 RIG_MIGRATION_DONE。');
  await page.wait(`[...document.querySelectorAll('.message[data-role=assistant] .message__body')].some(node=>node.innerText.includes('RIG_MIGRATION_DONE'))&&!document.querySelector('.danger-button')&&document.querySelectorAll('.agent-usage').length>0`);
  const sessionId = await page.evaluate(`window.__audit.sent.find(frame=>frame.method==='session/prompt').params.sessionId`);
  const first = await page.evaluate(`({usage:[...document.querySelectorAll('.agent-usage')].map(node=>node.textContent),samples:window.__audit.samples,frames:window.__audit.received})`);
  assert(first.samples.length > 0, 'Visible intermediate streaming state was captured');
  const updates = first.frames.flat().filter(frame=>frame.method==='session/update').map(frame=>frame.params.update);
  const tools = updates.filter(update=>update.sessionUpdate==='tool_call_update'&&update.status==='completed');
  assert(tools.length>=2, 'Two actual tools completed');
  assert(JSON.stringify(tools).includes('RIG_MIGRATION_ONE') && JSON.stringify(tools).includes('RIG_MIGRATION_TWO'), 'Both real tool outputs arrived');
  await writeFile(join(evidence, 'updates.json'), JSON.stringify(updates, null, 2));
  results.push({ scenario: 'two-tool-turns-and-streaming', sessionId, intermediateSamples: first.samples.length, usage: first.usage });
  await page.screenshot('tool-loop');
  await writeFile(join(evidence, 'stream-samples.json'), JSON.stringify(first.samples, null, 2));
  await page.call('Page.reload');
  await page.wait(`[...document.querySelectorAll('.message[data-role=assistant] .message__body')].some(node=>node.innerText.includes('RIG_MIGRATION_DONE'))&&document.querySelectorAll('.agent-usage').length>0`, 20000);
  assert.deepEqual(await page.evaluate(`[...document.querySelectorAll('.agent-usage')].map(node=>node.textContent)`), first.usage, 'Replay must not duplicate usage');
  results.push({ scenario: 'reconnect-usage', passed: true });

  await page.send('请使用 exec_command 执行 sleep 15; printf RIG_CANCEL_LATE。不要使用子代理。');
  await page.wait(`!!document.querySelector('.danger-button')`);
  await delay(500);
  const cancelledAt = Date.now();
  await page.click('停止');
  await page.wait(`!document.querySelector('.danger-button')`, 10000);
  results.push({ scenario: 'cancel', elapsedMs: Date.now() - cancelledAt });
  await page.send('请忽略被取消的任务，只回答 RIG_AFTER_CANCEL。不要调用工具。');
  await page.wait(`[...document.querySelectorAll('.message[data-role=assistant] .message__body')].some(node=>node.innerText.includes('RIG_AFTER_CANCEL'))&&!document.querySelector('.danger-button')`);
  results.push({ scenario: 'request-after-cancel', passed: true });

  await page.send('请执行 exec_command: sleep 3; printf RIG_QUEUE_TOOL。不要使用子代理。结束后回答 RIG_QUEUE_FIRST。');
  await page.wait(`!!document.querySelector('.danger-button')`);
  await page.send('只回答 RIG_QUEUED_DONE。不要调用工具。', '加入队列');
  await page.wait(`!!document.querySelector('.pending-queue')`, 10000);
  await page.evaluate(`document.querySelector('.composer-delivery__options button[aria-pressed=false]').click()`);
  await page.send('这是本轮补充：不要执行任何文件操作。', '补充本轮');
  await page.answer(sessionId,'RIG_QUEUED_DONE');
  results.push({scenario:'follow-up-and-steer',passed:true});

  await page.rpc('session/prompt', {sessionId,prompt:[
    {type:'text',text:'请读取这张附图；如果上下文提示模型不支持图片或图片被省略，只回答 RIG_IMAGE_FALLBACK。不要猜测图片内容，不要调用工具。'},
    {type:'image',mimeType:'image/png',data:'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZqkAAAAASUVORK5CYII='}
  ]});
  await page.answer(sessionId,'RIG_IMAGE_FALLBACK');
  results.push({scenario:'deepseek-image-fallback',passed:true});

  const compact = await page.rpc(`_${bootstrap.product.slug}/session/compact`, { sessionId });
  assert(compact.summary, 'Real summary returned');
  results.push({ scenario: 'real-compaction', usage: compact.usage });
  await page.send('请根据压缩后的历史，只回答 RIG_AFTER_COMPACT。不要调用工具。');
  await page.wait(`[...document.querySelectorAll('.message[data-role=assistant] .message__body')].some(node=>node.innerText.includes('RIG_AFTER_COMPACT'))&&!document.querySelector('.danger-button')`);
  results.push({ scenario: 'request-after-compaction', passed: true });
  const tree = await page.rpc(`_${bootstrap.product.slug}/session/tree`, { sessionId });
  await writeFile(join(evidence, 'tree.json'), JSON.stringify(tree, null, 2));
  assert(!JSON.stringify(tree).includes('model_message'), 'Tree excludes native provider payloads');
  results.push({ scenario: 'tree', available: true });
  const fork = await page.rpc(`_${bootstrap.product.slug}/session/fork`, { sessionId, entryId: tree.leafId, cwd:work });
  const forkReply = await page.rpc('session/prompt', { sessionId:fork.sessionId, prompt:[{type:'text',text:'只回答 RIG_FORK_OK。不要调用工具。'}] });
  assert(forkReply.messageId, 'Fork prompt inserted');
  await page.answer(fork.sessionId, 'RIG_FORK_OK');
  results.push({ scenario:'fork-continues',sessionId:fork.sessionId });
  await page.rpc('session/close', {sessionId:fork.sessionId});
  await page.rpc('session/close', {sessionId});
  await page.rpc('session/resume', {sessionId,cwd:work});
  const reopened = await page.rpc('session/prompt', {sessionId,prompt:[{type:'text',text:'只回答 RIG_REOPEN_OK。不要调用工具。'}]});
  assert(reopened.messageId, 'Reopened prompt inserted');
  await page.answer(sessionId, 'RIG_REOPEN_OK');
  results.push({scenario:'reopen-continues',passed:true});
  await page.screenshot('final');
  assert.deepEqual(page.errors, [], 'No uncaught browser errors');
  console.log(JSON.stringify({ results, errors: page.errors }, null, 2));
} catch (error) {
  results.push({ scenario: 'failure', error: String(error) });
  if (page) {
    await page.screenshot('failure').catch(() => {});
    await writeFile(join(evidence, 'failure-page.txt'), await page.evaluate('document.body.innerText').catch(() => 'unavailable'));
  }
  process.exitCode = 1;
  console.error(String(error));
} finally {
  await writeFile(join(evidence, 'results.json'), JSON.stringify({ results, errors: page?.errors ?? [] }, null, 2));
  page?.socket.close();
  browser.kill('SIGTERM');
  await new Promise(resolve => browser.once('exit', resolve));
  await rm(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
