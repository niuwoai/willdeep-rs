// Run against web_runtime_retry_fixture.mjs; only local synthetic tasks are used.
const { chromium } = require('playwright');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');

(async () => {
  const base = 'http://127.0.0.1:19849';
  const output = path.resolve('target/web-chat-stop');
  await fs.mkdir(output, { recursive: true });
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  const checks = [];
  try {
    for (const mode of ['waiting', 'pending']) {
      const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, locale: 'zh-CN' });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', error => errors.push(error.message));
      const state = async () => (await (await page.request.get(`${base}/__qa/state`)).json());
      const waitState = async predicate => {
        const deadline = Date.now() + 10000;
        while (Date.now() < deadline) {
          const value = await state();
          if (predicate(value)) return value;
          await new Promise(resolve => setTimeout(resolve, 50));
        }
        throw new Error('fixture state did not reach the expected condition');
      };
      assert.equal((await page.request.post(`${base}/__qa/phase?value=stopped`)).status(), 200);
      assert.equal((await page.request.post(`${base}/__qa/chat-mode?value=${mode}`)).status(), 200);
      const before = await state();
      await page.goto(base);
      await page.getByPlaceholder('描述你想完成的任务…').fill('本地停止测试');
      await page.getByRole('button', { name: '发送', exact: true }).click();
      const received = await waitState(value => value.chat && value.chat.turn !== before.chat?.turn);
      const turn = received.chat.turn;
      const stop = page.getByRole('button', { name: '停止生成', exact: true });
      assert.equal(await stop.isEnabled(), true);
      if (mode === 'waiting') {
        await page.getByText('等待重试 · 60s', { exact: true }).waitFor();
        await page.screenshot({ path: path.join(output, 'root-retry-wait.png'), fullPage: true });
      }
      await stop.click();
      if (mode === 'pending') {
        const pending = await state();
        assert.equal(pending.chat.submitted, false);
        assert.equal(pending.requests.filter(item => item.path === `/api/turns/${turn}/stop`).length, 0);
        assert.equal((await page.request.post(`${base}/__qa/submit-chat`)).status(), 200);
      }
      const stopped = await waitState(value => value.chat.turn === turn && value.chat.stopped && value.chat.closed);
      assert.equal(stopped.requests.filter(item => item.method === 'POST' && item.path === `/api/turns/${turn}/stop`).length, 1);
      await page.getByRole('button', { name: '发送', exact: true }).waitFor();
      assert.equal(await stop.count(), 0);
      await page.getByText('等待重试 · 60s', { exact: true }).waitFor({ state: 'hidden' });
      await page.screenshot({ path: path.join(output, `${mode}-stopped.png`), fullPage: true });
      assert.deepEqual(errors, []);
      checks.push(`${mode}: stop targets submitted turn once, aborts SSE, clears waiting and restores composer`);
      await context.close();
    }
    const report = { passed: true, scope: 'production React root chat against isolated local SSE fixture', checks };
    await fs.writeFile(path.join(output, 'report.json'), JSON.stringify(report, null, 2));
    await fs.writeFile(path.join(output, 'report.md'), '# 根聊天停止回归\n\n' + checks.map(check => `- PASS: ${check}`).join('\n') + '\n');
    console.log(JSON.stringify(report));
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
