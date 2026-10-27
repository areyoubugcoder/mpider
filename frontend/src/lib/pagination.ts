/** 统一分页边界与页码窗口，所有列表默认每页10条。 */
export const DEFAULT_PAGE_SIZE = 10;
export const PAGE_SIZES = [10, 20, 50, 100] as const;
export const pageCountFor = (total: number, size: number) => Math.max(1, Math.ceil(Math.max(0, total) / Math.max(1, size)));
export function pageNumbers(page: number, count: number): (number | "ellipsis")[] {
  count=Math.max(1,count);page=Math.max(0,Math.min(page,count-1));
  if(count<=7)return Array.from({length:count},(_,i)=>i);
  const visible=new Set([0,count-1,page-1,page,page+1].filter(n=>n>=0&&n<count));
  const result:(number|"ellipsis")[]=[];let last=-1;
  for(const n of [...visible].sort((a,b)=>a-b)){if(last>=0&&n-last>1)result.push("ellipsis");result.push(n);last=n;}
  return result;
}
