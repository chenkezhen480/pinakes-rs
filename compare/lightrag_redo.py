"""Files one page into a LightRAG partition again (a document that failed, e.g. on a 402):
lightrag_redo.py <tenant> <scope> <page id>; deletes the failed document first."""
import asyncio, sys
import corpus, sys_lightrag

async def main(t, scope, page_id):
    rag = await sys_lightrag.make(t, scope, "lightrag-ingest", cache=True)
    doc = f"{t}/{page_id}"
    print(await rag.adelete_by_doc_id(doc))
    p = next(p for p in corpus.pages(t) if p["id"] == page_id)
    await rag.ainsert([p["text"]], ids=[doc], file_paths=[p["id"]])
    print(await rag.doc_status.get_by_id(doc))
    await rag.finalize_storages()

asyncio.run(main(*sys.argv[1:4]))
