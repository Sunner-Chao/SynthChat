import { ChangeEvent, useRef, useState } from "react";
import { Camera, ChevronRight, Heart, Image, Newspaper, Pencil, Plus, Send, Trash2 } from "lucide-react";
import { api } from "../lib/api";
import { useAppStore } from "../lib/store";
import type { MomentComment, MomentPost } from "../lib/types";
export function MomentsPanel() {
  const {
    moments,
    createMoment,
    updateMomentText,
    addMomentComment,
    updateMomentComment,
    deleteMoment,
    deleteMomentComment,
    toggleMomentLike,
    uploadMomentCover,
    clearMomentCover,
    goBack
  } = useAppStore();
  const [draft, setDraft] = useState("");

  const submitPost = async () => {
    const body = draft.trim();
    if (!body) return;
    await createMoment(body);
    setDraft("");
  };

  return (
    <section className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <button className="icon-only-btn" onClick={goBack} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button>
        <div className="panel-title-text"><span>Moments</span><strong>朋友圈</strong></div>
      </div>
        <div className="composer">
          <textarea
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            placeholder="写一条朋友圈动态..."
          />
          <button onClick={submitPost} type="button">
            <Plus size={16} />
            发布
          </button>
        </div>
        {moments.length === 0 ? (
          <div className="empty-state">
            <Newspaper size={36} />
            <h2>朋友圈数据层已预留</h2>
            <p>0.1.8 的正文和评论纯数据编辑会在这里实现，保持时间戳和 feed 顺序。</p>
          </div>
        ) : (
          moments.map((post) => (
            <MomentCard
              key={post.id}
              post={post}
              onAddComment={addMomentComment}
              onDelete={deleteMoment}
              onDeleteComment={deleteMomentComment}
              onEdit={updateMomentText}
              onEditComment={updateMomentComment}
              onUploadCover={uploadMomentCover}
              onClearCover={clearMomentCover}
              onToggleLike={toggleMomentLike}
            />
          ))
        )}
    </section>
  );
}

function MomentCard({
  post,
  onAddComment,
  onDelete,
  onDeleteComment,
  onEdit,
  onEditComment,
  onUploadCover,
  onClearCover,
  onToggleLike
}: {
  post: MomentPost;
  onAddComment: (postId: string, text: string) => Promise<void>;
  onDelete: (postId: string) => Promise<void>;
  onDeleteComment: (postId: string, commentId: string) => Promise<void>;
  onEdit: (postId: string, body: string) => Promise<void>;
  onEditComment: (postId: string, commentId: string, text: string) => Promise<void>;
  onUploadCover: (postId: string, file: File) => Promise<void>;
  onClearCover: (postId: string) => Promise<void>;
  onToggleLike: (postId: string) => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [body, setBody] = useState(post.body);
  const [commentDraft, setCommentDraft] = useState("");
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const liked = post.likedBy.includes("user");

  const saveBody = async () => {
    await onEdit(post.id, body);
    setEditing(false);
  };

  const submitComment = async () => {
    const text = commentDraft.trim();
    if (!text) return;
    await onAddComment(post.id, text);
    setCommentDraft("");
  };

  const uploadCover = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    await onUploadCover(post.id, file);
  };

  return (
    <article className="moment-card" data-avatar={post.personaId.slice(0, 1).toUpperCase()}>
      <div className="moment-head">
        <div>
          <strong>{post.personaId}</strong>
          <span>{new Date(post.createdAt).toLocaleString()}</span>
        </div>
        <button className="icon-button danger" onClick={() => void onDelete(post.id)} type="button" title="删除动态">
          <Trash2 size={16} />
        </button>
      </div>

      {post.coverPath ? (
        <div className="moment-cover">
          <img alt="朋友圈封面" src={api.assetUrl(post.coverPath)} />
          <button onClick={() => void onClearCover(post.id)} type="button">
            清除封面
          </button>
        </div>
      ) : null}

      {editing ? (
        <div className="inline-editor">
          <textarea value={body} onChange={(event) => setBody(event.target.value)} />
          <div className="editor-actions">
            <button onClick={saveBody} type="button">保存</button>
            <button onClick={() => setEditing(false)} type="button">取消</button>
          </div>
        </div>
      ) : (
        <p>{post.body}</p>
      )}

      <div className="moment-actions">
        <button className={liked ? "text-button liked" : "text-button"} onClick={() => void onToggleLike(post.id)} type="button">
          <Heart size={15} />
          {post.likedBy.length}
        </button>
        <button className="text-button" onClick={() => setEditing(true)} type="button">
          <Pencil size={15} />
          编辑正文
        </button>
        <button className="text-button" onClick={() => fileInputRef.current?.click()} type="button">
          <Image size={15} />
          上传封面
        </button>
        <input
          accept="image/png,image/jpeg,image/webp,image/gif"
          className="hidden-input"
          onChange={uploadCover}
          ref={fileInputRef}
          type="file"
        />
      </div>

      <div className="comment-list">
        {post.comments.map((comment) => (
          <MomentCommentRow
            comment={comment}
            key={comment.id}
            postId={post.id}
            onDelete={onDeleteComment}
            onEdit={onEditComment}
          />
        ))}
      </div>

      <div className="comment-composer">
        <input
          value={commentDraft}
          onChange={(event) => setCommentDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") void submitComment();
          }}
          placeholder="写评论..."
        />
        <button onClick={submitComment} type="button" title="发送评论">
          <Send size={16} />
        </button>
      </div>
    </article>
  );
}

function MomentCommentRow({
  comment,
  postId,
  onDelete,
  onEdit
}: {
  comment: MomentComment;
  postId: string;
  onDelete: (postId: string, commentId: string) => Promise<void>;
  onEdit: (postId: string, commentId: string, text: string) => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(comment.text);

  const save = async () => {
    await onEdit(postId, comment.id, text);
    setEditing(false);
  };

  return (
    <div className="comment-row">
      <div>
        <strong>{comment.personaId}</strong>
        {editing ? (
          <input value={text} onChange={(event) => setText(event.target.value)} />
        ) : (
          <span>{comment.text}</span>
        )}
      </div>
      <div className="comment-actions">
        {editing ? (
          <button onClick={save} type="button">保存</button>
        ) : (
          <button onClick={() => setEditing(true)} type="button">编辑</button>
        )}
        <button onClick={() => void onDelete(postId, comment.id)} type="button">删除</button>
      </div>
    </div>
  );
}

