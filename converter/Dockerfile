# Pinakes' document converter (design.md, "Files are turned into Markdown first").
FROM python:3.12-slim

# Pinned: the converter's version is recorded with every document version it converts.
RUN pip install --no-cache-dir "markitdown[docx,pptx,xlsx,xls,pdf]==0.1.8"

COPY server.py /app/server.py

# No root, and nothing to write: the compose file also mounts the file system read-only.
USER nobody
ENV PYTHONDONTWRITEBYTECODE=1
EXPOSE 8090
CMD ["python", "/app/server.py"]
