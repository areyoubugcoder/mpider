import { siteUrl } from '../lib/site';

export function GET() {
  return new Response(`User-agent: *\nAllow: /\n\nSitemap: ${siteUrl('sitemap-index.xml')}\n`, {
    headers: { 'Content-Type': 'text/plain; charset=utf-8' },
  });
}
