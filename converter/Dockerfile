# Pinakes' optional document converter (MarkItDown), for the formats the service doesn't read itself.
FROM python:3.12-slim

# Pinned: the converter's version is recorded with every document version it converts.
RUN pip install --no-cache-dir "markitdown[docx,pptx,xlsx,xls,pdf]==0.1.8"

COPY server.py /app/server.py

# No root, and nothing to write: the compose file also mounts the file system read-only.
USER nobody
ENV PYTHONDONTWRITEBYTECODE=1
EXPOSE 8090
# Healthy once it answers: the server listens only after MarkItDown has loaded, which takes a few
# seconds, and Pinakes starts only then (compose `depends_on: service_healthy`). The image has
# no curl, so Python asks.
HEALTHCHECK --interval=2s --timeout=3s --start-period=5s --retries=30 \
    CMD ["python", "-c", "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8090/health', timeout=2)"]
CMD ["python", "/app/server.py"]
