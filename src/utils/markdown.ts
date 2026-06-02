/**
 * Markdown 渲染工具
 * 統一處理 Markdown 轉 HTML，包含程式碼高亮
 */
import { marked, Renderer } from 'marked';
import hljs from 'highlight.js';

// 自訂 renderer 來處理程式碼高亮
const renderer = new Renderer();
renderer.code = function ({ text, lang }: { text: string; lang?: string }) {
  const language = lang && hljs.getLanguage(lang) ? lang : 'plaintext';
  const highlighted = hljs.highlight(text, { language }).value;
  return `<pre><code class="hljs language-${language}">${highlighted}</code></pre>`;
};

// 設定 marked
marked.setOptions({
  breaks: true, // 支援換行
});
marked.use({ renderer });

/**
 * 將 Markdown 轉換為 HTML
 * @param content Markdown 內容
 * @returns HTML 字串
 */
export function renderMarkdown(content: string): string {
  return marked.parse(content, { async: false }) as string;
}

/**
 * 將 Markdown 轉換為純文字（移除 HTML 標籤）
 * @param content Markdown 內容
 * @returns 純文字
 */
export function markdownToPlainText(content: string): string {
  const html = renderMarkdown(content);
  // 簡單移除 HTML 標籤
  return html.replace(/<[^>]*>/g, '').trim();
}
