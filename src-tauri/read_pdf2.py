import sys
sys.stdout.reconfigure(encoding='utf-8')
import pdfplumber

pdf_path = r'C:\Users\33908\AppData\Roaming\cc.synthchat.v1\synthchat-data\attachments\attachment-1833391673da45b69cbe0416c7ebbd74-AI____AIGC_______.pdf'
with pdfplumber.open(pdf_path) as pdf:
    print(f"页数: {len(pdf.pages)}")
    for i, page in enumerate(pdf.pages):
        text = page.extract_text() or ""
        tables = page.extract_tables()
        print(f"\n=== Page {i} (text_len={len(text)}, tables={len(tables)}) ===")
        if text:
            print(text[:3000])
        else:
            # Check if page has images
            images = page.images
            print(f"  images count: {len(images)}")
            # Try to get page dimensions
            print(f"  page size: {page.width} x {page.height}")
            # Check for any raw content
            if hasattr(page, 'chars') and page.chars:
                print(f"  chars count: {len(page.chars)}")
            else:
                print("  No chars found - likely image-based PDF")
