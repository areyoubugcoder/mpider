import { getCollection } from 'astro:content';

export async function publishedPosts() {
  const now = new Date();
  const posts = await getCollection('blog', ({ data }) => !data.draft && data.pubDate <= now);
  return posts.sort((a, b) => b.data.pubDate.valueOf() - a.data.pubDate.valueOf() || a.id.localeCompare(b.id));
}
