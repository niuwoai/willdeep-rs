import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

const remarkPlugins = [remarkGfm];

const components: Components = {
  a({ node, ...props }) {
    void node;
    return <a {...props} target="_blank" rel="noopener noreferrer" />;
  },
};

export function Markdown({ content }: { content: string }) {
  return <div className="markdown-body">
    <ReactMarkdown remarkPlugins={remarkPlugins} components={components}>{content}</ReactMarkdown>
  </div>;
}
