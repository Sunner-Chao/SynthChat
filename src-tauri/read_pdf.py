import sys
sys.stdout.reconfigure(encoding='utf-8')
import pdfplumber

pdf_path = r'C:\Users\33908\AppData\Roaming\cc.synthchat.v1\synthchat-data\attachments\attachment-1833391673da45b69cbe0416c7ebbd74-AI____AIGC_______.pdf'
with pdfplumber.open(pdf_path) as pdf:
    print(f"页数: {len(pdf.pages)}")
    for i, page in enumerate(pdf.pages):
        text = page.extract_text() or ""
        print(f"\n=== Page {i} (text_len={len(text)}) ===")
        print(text[:3000])
