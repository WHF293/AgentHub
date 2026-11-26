import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import './markdown.css';

/** 记忆文件 Markdown 渲染（GFM 表格/任务列表；不渲染内联 HTML，天然防注入） */
export function MarkdownView({ content }: { content: string }) {
  return (
    <div className="md-view">
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{content}</ReactMarkdown>
    </div>
  );
}
