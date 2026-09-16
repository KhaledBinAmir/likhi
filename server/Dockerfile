# Likhi telemetry ingest on Cloud Run.
#   gcloud run deploy likhi-ingest --source server --region us-central1 \
#     --set-env-vars LIKHI_INGEST_BUCKET=YOUR_BUCKET,LIKHI_INGEST_KEY=YOUR_SECRET \
#     --allow-unauthenticated --min-instances 0
#
# --min-instances 0 lets it scale to zero, so an idle pilot costs nothing.
FROM python:3.12-slim

WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY ingest_server.py .

# Cloud Run sets PORT; the server reads it.
ENV PORT=8080
EXPOSE 8080
CMD ["python", "ingest_server.py"]
