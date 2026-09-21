// Run against web_runtime_retry_fixture.mjs; only local synthetic turns are used.
// 轮次结束后的下一句预测：灰字出现、Tab 只填入不发送、打字 / Esc 放弃且不回来、
// 刷新后不复现、三种语言的提示文案。
const { chromium } = require('playwright');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');

const base = 'http://127.0.0.1:19849';
const SUGGESTION = '提交并合回 develop';
const HINTS = { 'zh-CN': 'Tab 采用', en: 'Tab to accept', ja: 'Tab で採用' };

(async () => {
  const output = path.resolve('target/web-input-suggestion');
  await fs.mkdir(output, { recursive: true });
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  const checks = [];
  const post = (page, route) => page.request.post(`${base}${route}`);
  const state = async page => (await (await page.request.get(`${base}/__qa/state`)).json());
  const count = (value, predicate) => value.requests.filter(predicate).length;
  const isChat = item => item.method === 'POST' && item.path === '/api/chat/stream';
  const isSuggestion = item => item.method === 'POST' && item.path.endsWith('/input-suggestion');

  async function openPage(locale) {
    const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(base);
    const input = page.locator('.composer-shell textarea');
    await input.waitFor();
    const ghost = page.locator('.input-suggestion');
    return { context, page, errors, input, ghost };
  }

  // 发一轮并等它收尾：Enter 发送，fixture 立刻回 submitted + completed。
  async function finishTurn({ page, input }, text) {
    const before = await state(page);
    await input.fill(text);
    await input.press('Enter');
    const deadline = Date.now() + 10000;
    while (Date.now() < deadline) {
      const value = await state(page);
      if (count(value, isSuggestion) > count(before, isSuggestion)) return value;
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    throw new Error('the finished turn did not ask for a suggestion');
  }

  try {
    const setup = await browser.newContext();
    const control = await setup.newPage();
    assert.equal((await post(control, '/__qa/chat-mode?value=complete')).status(), 200);
    assert.equal((await post(control, `/__qa/suggestion?value=${encodeURIComponent(SUGGESTION)}`)).status(), 200);

    {
      const view = await openPage('zh-CN');
      const { page, input, ghost, errors } = view;

      const asked = await finishTurn(view, '把测试修好');
      const request = asked.requests.filter(isSuggestion).at(-1);
      assert.equal(request.turn_id, asked.chat.turn, '请求带上刚收尾那一轮的 turn_id');
      assert.equal(request.path, `/api/sessions/${asked.chat.session}/input-suggestion`);
      await ghost.getByText(SUGGESTION, { exact: true }).waitFor();
      await ghost.getByText(HINTS['zh-CN'], { exact: true }).waitFor();
      assert.equal(await input.inputValue(), '', '灰字不是输入框的值');
      await page.screenshot({ path: path.join(output, 'ghost-visible.png'), fullPage: true });
      checks.push('完成一轮后空输入框出现灰字预测与「Tab 采用」，请求带 turn_id');

      const chatsBefore = count(await state(page), isChat);
      await input.focus();
      await page.keyboard.press('Tab');
      assert.equal(await input.inputValue(), SUGGESTION);
      await ghost.waitFor({ state: 'hidden' });
      await page.waitForTimeout(300);
      assert.equal(count(await state(page), isChat), chatsBefore, 'Tab 只填入，不发送');
      assert.equal(await input.evaluate(element => element === document.activeElement), true, 'Tab 没把焦点带走');
      checks.push('Tab 把预测填进输入框、焦点留在原地，且没有发出 /api/chat/stream');

      await input.fill('');
      await page.waitForTimeout(300);
      assert.equal(await ghost.count(), 0, '采用后清空也不回来');

      await finishTurn(view, '第二轮');
      await ghost.getByText(SUGGESTION, { exact: true }).waitFor();
      await input.pressSequentially('x');
      await ghost.waitFor({ state: 'hidden' });
      await input.fill('');
      await page.waitForTimeout(300);
      assert.equal(await ghost.count(), 0, '打字放弃后删回空也不回来');
      checks.push('打字即清掉灰字，删回空也不复现');

      await finishTurn(view, '第三轮');
      await ghost.getByText(SUGGESTION, { exact: true }).waitFor();
      await input.focus();
      await page.keyboard.press('Escape');
      await ghost.waitFor({ state: 'hidden' });
      assert.equal(await input.inputValue(), '');
      checks.push('Esc 清掉灰字，输入框保持为空');

      await finishTurn(view, '第四轮');
      await ghost.getByText(SUGGESTION, { exact: true }).waitFor();
      await page.reload();
      await input.waitFor();
      await page.waitForTimeout(500);
      assert.equal(await ghost.count(), 0, '刷新后不复现');
      checks.push('刷新页面后预测不复现（不持久化）');

      assert.equal((await post(page, '/__qa/suggestion?value=null')).status(), 200);
      await finishTurn(view, '第五轮');
      await page.waitForTimeout(300);
      assert.equal(await ghost.count(), 0);
      checks.push('服务端回 null 时不显示任何东西');
      assert.equal((await post(page, `/__qa/suggestion?value=${encodeURIComponent(SUGGESTION)}`)).status(), 200);

      assert.deepEqual(errors, []);
      await view.context.close();
    }

    for (const locale of ['en', 'ja']) {
      const view = await openPage(locale);
      await finishTurn(view, 'fix the tests');
      await view.ghost.getByText(HINTS[locale], { exact: true }).waitFor();
      await view.page.screenshot({ path: path.join(output, `ghost-${locale}.png`), fullPage: true });
      assert.deepEqual(view.errors, []);
      await view.context.close();
    }
    checks.push('英文与日文界面的提示分别为「Tab to accept」「Tab で採用」');

    assert.equal((await post(control, '/__qa/chat-mode?value=waiting')).status(), 200);
    await setup.close();

    const report = { passed: true, scope: 'production React composer against isolated local SSE fixture', checks };
    await fs.writeFile(path.join(output, 'report.json'), JSON.stringify(report, null, 2));
    await fs.writeFile(path.join(output, 'report.md'), '# Web 下一句预测回归\n\n' + checks.map(check => `- PASS: ${check}`).join('\n') + '\n');
    console.log(JSON.stringify(report));
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
