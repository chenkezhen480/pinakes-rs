// The console's main flows in a real browser, against the seeded test service (one tenant,
// a personal deployment): signing in with the default account, reading a rendered page, adding
// a document and following its job, drawing the graph, approving a draft and a held edit,
// editing a setting, turning embeddings on, and walking the Library's hall. They run in order on
// one service, in the list layout except where the hall is the point.
import { expect, test, type Page } from "@playwright/test";

test.describe.configure({ mode: "serial" });

/** Every page is checked for errors in the browser console (CSP violations included). */
function watch(page: Page): string[] {
  const problems: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") problems.push(m.text());
  });
  page.on("pageerror", (e) => problems.push(e.message));
  return problems;
}

/** The list layout for this page (the hall is tested on its own). */
async function listLayout(page: Page) {
  await page.addInitScript(() => localStorage.setItem("pinakes.layout", "list"));
}

async function signInForm(page: Page, user: string, password: string) {
  await page.getByLabel("用户名").fill(user);
  await page.getByLabel("密码").fill(password);
  await page.getByRole("button", { name: "登录" }).click();
}

test("the default account signs in and stays signed in", async ({ page }) => {
  const problems = watch(page);
  await listLayout(page);
  await page.goto("/ui/");
  await expect(page.getByText("首次登录：用户名 root，密码 1234。")).toBeVisible();
  await signInForm(page, "root", "wrong");
  await expect(page.getByRole("alert")).toContainText("用户名或密码不对");
  await signInForm(page, "root", "1234");
  await expect(page.getByRole("heading", { name: "总览" })).toBeVisible();
  await expect(page.locator(".caller")).toContainText("000000");
  await expect(page.getByText("已关闭 · 仅关键词")).toBeVisible();
  // Kept in the browser: the next visit is signed in.
  await page.reload();
  await expect(page.getByRole("heading", { name: "总览" })).toBeVisible();
  await page.getByRole("button", { name: "退出" }).click();
  await expect(page.getByRole("button", { name: "登录" })).toBeVisible();
  await page.reload();
  await expect(page.getByRole("button", { name: "登录" })).toBeVisible();
  // The browser logs the wrong password's 401; nothing else.
  expect(problems.filter((p) => !p.includes("status of 401"))).toEqual([]);
});

test.describe("signed in", () => {
  // Each test's browser starts with a session, as after signing in.
  test.beforeEach(async ({ page, request }) => {
    const resp = await request.post("/console/sign-in", { data: { user: "root", password: "1234" } });
    expect(resp.ok()).toBe(true);
    const { token } = (await resp.json()) as { token: string };
    await page.addInitScript((t) => {
      localStorage.setItem("pinakes.console", JSON.stringify({ token: t, tenant: "", user: "", groups: "" }));
    }, token);
    await listLayout(page);
  });

  test("the hall is walked: a station opens its page on a folio, and back", async ({ page }) => {
    const problems = watch(page);
    await page.addInitScript(() => localStorage.setItem("pinakes.layout", "hall"));
    await page.goto("/ui/#/?sky=night");
    await expect(page.locator(".stage.ready canvas")).toBeVisible({ timeout: 30_000 });
    await expect(page.locator(".bar .sky")).toContainText(/\d{2}:\d{2} · 星空/);
    // The dock names every station; choosing one walks there and opens its page.
    const dock = page.getByRole("navigation", { name: "栏目" });
    await dock.getByRole("link", { name: "图谱" }).click();
    await expect(page.getByRole("heading", { level: 1, name: "图谱" })).toBeVisible();
    await expect(page.locator(".folio .where")).toHaveText("浑天仪");
    // Escape walks back to the entrance.
    await page.keyboard.press("Escape");
    await expect(page.locator(".folio")).toHaveCount(0);
    await expect(dock).toBeVisible();
    // Whom the knowledge is looked at as is chosen from the hall's bar too.
    await page.locator(".bar .who").click();
    await expect(page.locator("dialog.viewer-dialog")).toBeVisible();
    await page.locator("dialog.viewer-dialog").getByRole("button", { name: "取消" }).click();
    await expect(page.locator("dialog.viewer-dialog")).toBeHidden();
    // And the list is a click away, remembered.
    await page.getByRole("button", { name: "列表视图" }).click();
    await expect(page.locator(".rail")).toBeVisible();
    expect(await page.evaluate(() => localStorage.getItem("pinakes.layout"))).toBe("list");
    expect(problems.filter((p) => !/GPU stall|WebGL/i.test(p))).toEqual([]);
  });

  test("a page is read rendered, and nothing it holds runs", async ({ page }) => {
    const problems = watch(page);
    let alerted = false;
    page.on("dialog", (d) => {
      alerted = true;
      void d.dismiss();
    });
    await page.goto("/ui/#/wiki");
    await page.getByRole("link", { name: "退货政策" }).click();
    await expect(page.getByRole("heading", { level: 1, name: "退货政策" })).toBeVisible();
    const prose = page.locator(".prose");
    await expect(prose.locator("table")).toContainText("银行卡");
    await expect(prose.locator("strong")).toHaveText("七天");
    await expect(prose).toContainText("<script>alert('x')</script>");
    expect(alerted).toBe(false);
    // A wiki link opens the page it names.
    await prose.getByRole("link", { name: "expertise/refund-times" }).click();
    await expect(page.getByRole("heading", { level: 1, name: "退款时限" })).toBeVisible();
    expect(problems).toEqual([]);
  });

  test("a document is uploaded and its job followed to the page it made", async ({ page }) => {
    await page.goto("/ui/#/knowledge");
    await page.locator("input[type=file]").setInputFiles({
      name: "会员说明.md",
      mimeType: "text/markdown",
      buffer: Buffer.from("# 会员说明\n\n金卡每月两张免运费券。\n"),
    });
    const sent = page.locator(".sent li").first();
    await expect(sent).toContainText("会员说明.md");
    await expect(sent).toContainText("新建 1 页", { timeout: 20_000 });
    await sent.getByRole("link", { name: "expertise/uploaded-document" }).click();
    await expect(page.getByRole("heading", { level: 1, name: "会员说明" })).toBeVisible();
  });

  test("a format the service can't read is refused with what to do", async ({ page }) => {
    await page.goto("/ui/#/knowledge");
    await page.locator("input[type=file]").setInputFiles({ name: "report.pdf", mimeType: "application/pdf", buffer: Buffer.from("%PDF-1.7") });
    await expect(page.locator(".sent li").first()).toContainText("docker compose --profile markitdown up -d converter");
  });

  test("the graph is browsed and drawn", async ({ page }) => {
    const problems = watch(page);
    await page.goto("/ui/#/graph/node/mainland:order/1042");
    const canvas = page.locator(".canvas svg");
    await expect(canvas.getByRole("button", { name: "订单 1042 (order)" })).toBeVisible();
    await expect(canvas.getByRole("button", { name: "林晓 (customer)" })).toBeVisible();
    await expect(canvas.locator(".edge text")).toHaveText("placed_by");
    // Choosing a neighbour walks from it.
    await canvas.getByRole("button", { name: "林晓 (customer)" }).click();
    await expect(page).toHaveURL(/graph\/node\/mainland:customer\/c1/);
    await expect(page.locator(".detail h2")).toHaveText("林晓");
    expect(problems).toEqual([]);
  });

  test("a graph draft, a held edit and an external source are approved", async ({ page }) => {
    await page.goto("/ui/#/review");
    const items = page.locator(".proposal");
    await expect(items).toHaveCount(3);
    const draft = items.filter({ hasText: "图谱草稿" });
    await expect(draft.locator(".canvas svg").getByRole("button", { name: "王芳 (customer)" })).toBeVisible();
    await draft.getByRole("button", { name: "批准" }).click();
    await expect(items).toHaveCount(2);
    const held = items.filter({ hasText: "暂扣修改" });
    await expect(held).toContainText("a person should look at this");
    await expect(held.locator(".diff .add")).toContainText("HOLD 现金到付 7 天");
    await held.getByRole("button", { name: "批准" }).click();
    await expect(items).toHaveCount(1);
    // The source: its gap, address and screening shown; approved, it is filed as a document.
    const source = items.filter({ hasText: "外部来源" });
    await expect(source).toContainText("海外退货");
    await expect(source).toContainText("help.example.com/returns-abroad");
    await expect(source).toContainText("Return windows for orders shipped abroad.");
    await source.getByRole("button", { name: "批准" }).click();
    await expect(page.getByText("没有待审内容")).toBeVisible({ timeout: 20_000 });
    // Filed by a job in the background: the page is opened again until the job has written it.
    await expect(async () => {
      await page.goto("/ui/#/wiki/expertise/returns-abroad");
      await page.reload();
      await expect(page.getByRole("heading", { level: 1, name: "Returns abroad" })).toBeVisible({ timeout: 1_000 });
    }).toPass({ timeout: 30_000 });
    // The draft is in the graph now.
    await page.goto("/ui/#/graph/node/mainland:customer/c2");
    await expect(page.locator(".detail h2")).toHaveText("王芳");
  });

  test("a setting is explained, saved to the file and waits for a restart", async ({ page }) => {
    await page.goto("/ui/#/settings/config");
    const row = page.locator(".field-row").filter({ has: page.locator("code.key", { hasText: /^retention_days$/ }) });
    await expect(row).toContainText("超过这么多天的轨迹文件会被删除");
    await row.locator("input").fill("7");
    await page.getByRole("button", { name: "保存" }).click();
    await expect(page.getByText("已保存的改动要在服务重启后才生效。")).toBeVisible();
    await expect(row).toContainText("重启前服务仍按 30 运行");
    // A field naming a secret's variable is shown, not offered for editing.
    const fixed = page.locator(".field-row").filter({ has: page.locator("code.key", { hasText: /^admin_key_env$/ }) });
    await expect(fixed).toContainText("只能在配置文件里手动修改");
    await expect(fixed.locator("input")).toHaveCount(0);
  });

  test("turning embeddings on downloads the model in the background", async ({ page }) => {
    await page.goto("/ui/#/settings/embedding");
    await expect(page.getByText("开启之前")).toBeVisible();
    await page.getByRole("switch").click();
    await expect(page.getByRole("progressbar")).toContainText("下载 onnx/model.onnx");
    await expect(page.getByText("已开启：所有已打开租户都可语义检索。")).toBeVisible({ timeout: 20_000 });
    await page.goto("/ui/#/search");
    await expect(page.locator(".modes label.off")).toHaveCount(0);
  });

  // Last: it changes the password the others sign in with.
  test("the account is changed in Settings, and other sessions end", async ({ page }) => {
    await page.goto("/ui/#/settings/environment");
    await expect(page.getByText("密码仍是默认的 1234")).toBeVisible();
    await page.getByLabel("当前密码").fill("1234");
    await page.getByLabel("新密码", { exact: true }).fill("e2e-new-secret");
    await page.getByLabel("再输一次新密码").fill("e2e-new-secret");
    await page.getByRole("button", { name: "修改" }).click();
    await expect(page.getByText("已修改，其他会话已退出。")).toBeVisible();
    await expect(page.getByText("密码仍是默认的 1234")).toHaveCount(0);
    // This browser goes on; signing in again takes the new password.
    await page.getByRole("button", { name: "退出" }).click();
    await signInForm(page, "root", "1234");
    await expect(page.getByRole("alert")).toContainText("用户名或密码不对");
    await signInForm(page, "root", "e2e-new-secret");
    await expect(page.getByText("当前账号：root · 已在这里修改")).toBeVisible();
  });
});
