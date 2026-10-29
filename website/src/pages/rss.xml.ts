import rss from '@astrojs/rss';
import type { APIContext } from 'astro';
import { publishedPosts } from '../lib/posts';
import { sitePath } from '../lib/site';

export async function GET(context: APIContext) {
  const posts = await publishedPosts();
  return rss({
    title: 'MPider 使用指南',
    description: '公众号文章本地保存、阅读整理与使用风险说明。',
    site: context.site!,
    items: posts.map((post) => ({
      title: post.data.title,
      description: post.data.description,
      pubDate: post.data.pubDate,
      link: sitePath(`blog/${post.id}/`),
    })),
    customData: '<language>zh-CN</language>',
  });
}
