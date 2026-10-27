/** 统一分页条及本地分页：始终显示总条数、每页条数与数字页码。 */
import { useEffect, useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { DEFAULT_PAGE_SIZE, PAGE_SIZES, pageCountFor, pageNumbers } from "@/lib/pagination";
export function Pager({page,pageCount,total,unit="条",busy=false,onPage,pageSize=DEFAULT_PAGE_SIZE,onPageSize,label="列表"}:{page:number;pageCount?:number;total:number;unit?:string;busy?:boolean;onPage:(page:number)=>void;pageSize?:number;onPageSize?:(size:number)=>void;label?:string}) {
  const count=pageCount??pageCountFor(total,pageSize);const current=Math.max(0,Math.min(page,count-1));
  return <nav aria-label={`${label}分页`} className="flex flex-wrap items-center justify-between gap-3 border-t pt-3 text-xs">
    <span className="whitespace-nowrap text-muted-foreground">共 {total} {unit} · 第 {current+1} / {count} 页</span>
    <div className="flex flex-wrap items-center gap-1">
      {onPageSize ? <select aria-label={`${label}每页条数`} className="mr-2 h-8 rounded-md border bg-background px-2 text-xs" value={pageSize} disabled={busy} onChange={e=>onPageSize(Number(e.target.value))}>{PAGE_SIZES.map(size=><option key={size} value={size}>{size} {unit}/页</option>)}</select> : <span className="mr-2 text-muted-foreground">{pageSize} {unit}/页</span>}
      <Button size="sm" variant="ghost" disabled={busy||current===0} onClick={()=>onPage(current-1)}>上一页</Button>
      {pageNumbers(current,count).map((n,i)=>n==="ellipsis"?<span key={`gap-${i}`} className="px-1 text-muted-foreground">…</span>:<Button key={n} size="sm" variant="ghost" className={`min-w-8 px-2 ${n===current?"bg-muted font-semibold":""}`} aria-label={`第 ${n+1} 页`} aria-current={n===current?"page":undefined} disabled={busy||n===current} onClick={()=>onPage(n)}>{n+1}</Button>)}
      <Button size="sm" variant="ghost" disabled={busy||current+1>=count} onClick={()=>onPage(current+1)}>下一页</Button>
    </div>
  </nav>;
}
export function usePaged<T>(rows:T[],size=DEFAULT_PAGE_SIZE,resetKey?:unknown){
  const [page,setPage]=useState(0);const pageCount=pageCountFor(rows.length,size);const safePage=Math.max(0,Math.min(page,pageCount-1));
  useEffect(()=>{setPage(0)},[size,resetKey]);
  useEffect(()=>{if(page!==safePage)setPage(safePage)},[page,safePage]);
  const slice=useMemo(()=>rows.slice(safePage*size,(safePage+1)*size),[rows,safePage,size]);
  return {page:safePage,pageCount,slice,setPage,total:rows.length};
}
export function useServerPage(total:number|undefined,size:number,page:number,setPage:(page:number)=>void){
  useEffect(()=>{if(total!==undefined&&page>=pageCountFor(total,size))setPage(pageCountFor(total,size)-1)},[total,size,page,setPage]);
}
