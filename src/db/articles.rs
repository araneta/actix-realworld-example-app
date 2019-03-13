use actix::prelude::*;
use blob_uuid::{to_blob, to_uuid};
use diesel::prelude::*;
use slug::slugify;
use uuid::Uuid;

use super::{DbExecutor, PooledConn};
use crate::app::articles::{
    ArticleListResponse, ArticleResponse, ArticleResponseInner, CreateArticleOuter, DeleteArticle,
    FavoriteArticle, GetArticle, GetArticles, GetFeed, UnfavoriteArticle, UpdateArticleOuter,
};
use crate::app::profiles::ProfileResponseInner;
use crate::models::{
    Article, ArticleChange, ArticleTag, NewArticle, NewArticleTag, NewFavoriteArticle, User,
};
use crate::prelude::*;
use crate::utils::CustomDateTime;

// message handler implementations ↓

impl Message for CreateArticleOuter {
    type Result = Result<ArticleResponse>;
}

impl Handler<CreateArticleOuter> for DbExecutor {
    type Result = Result<ArticleResponse>;

    fn handle(&mut self, msg: CreateArticleOuter, _: &mut Self::Context) -> Self::Result {
        use crate::schema::articles;

        let conn = &self.0.get()?;

        let author = msg.auth.user;

        // Generating the Uuid here since it will help make a unique slug
        // This is for when some articles may have similar titles such that they generate the same slug
        let new_article_id = Uuid::new_v4();
        let slug = generate_slug(&new_article_id, &msg.article.title);

        let new_article = NewArticle {
            id: new_article_id,
            author_id: author.id,
            slug,
            title: msg.article.title,
            description: msg.article.description,
            body: msg.article.body,
        };
        let article = diesel::insert_into(articles::table)
            .values(&new_article)
            .get_result::<Article>(conn)?;

        let inserted_tags = replace_tags(article.id, msg.article.tag_list, conn)?;
        let tags = inserted_tags
            .iter()
            .map(|article_tag| article_tag.tag_name.to_owned())
            .collect::<Vec<String>>();

        Ok(ArticleResponse {
            article: ArticleResponseInner {
                slug: article.slug,
                title: article.title,
                description: article.description,
                body: article.body,
                tag_list: tags,
                created_at: CustomDateTime(article.created_at),
                updated_at: CustomDateTime(article.updated_at),
                favorited: false,
                favorites_count: 0,
                author: ProfileResponseInner {
                    username: author.username,
                    bio: author.bio,
                    image: author.image,
                    following: false, // <- note you can't follow yourself
                },
            },
        })
    }
}

impl Message for GetArticle {
    type Result = Result<ArticleResponse>;
}

impl Handler<GetArticle> for DbExecutor {
    type Result = Result<ArticleResponse>;

    fn handle(&mut self, msg: GetArticle, _: &mut Self::Context) -> Self::Result {
        use crate::schema::{articles, followers, users};

        let conn = &self.0.get()?;

        let (article, author) = articles::table
            .inner_join(users::table)
            .filter(articles::slug.eq(msg.slug))
            .get_result::<(Article, User)>(conn)?;

        let (favorited, following) = match msg.auth {
            Some(auth) => get_favorited_and_following(article.id, author.id, auth.user.id, conn)?,
            None => (false, false),
        };

        let favorites_count = get_favorites_count(article.id, conn)?;

        let tags = select_tags_on_article(article.id, conn)?;

        Ok(ArticleResponse {
            article: ArticleResponseInner {
                slug: article.slug,
                title: article.title,
                description: article.description,
                body: article.body,
                tag_list: tags,
                created_at: CustomDateTime(article.created_at),
                updated_at: CustomDateTime(article.updated_at),
                favorited,
                favorites_count,
                author: ProfileResponseInner {
                    username: author.username,
                    bio: author.bio,
                    image: author.image,
                    following,
                },
            },
        })
    }
}

impl Message for UpdateArticleOuter {
    type Result = Result<ArticleResponse>;
}

impl Handler<UpdateArticleOuter> for DbExecutor {
    type Result = Result<ArticleResponse>;

    fn handle(&mut self, msg: UpdateArticleOuter, _: &mut Self::Context) -> Self::Result {
        use crate::schema::articles;

        let conn = &self.0.get()?;

        let article = articles::table
            .filter(articles::slug.eq(msg.slug))
            .get_result::<Article>(conn)?;

        if msg.auth.user.id != article.author_id {
            return Err(Error::Forbidden(json!({
                "error": "user is not the author of article in question",
            })));
        }

        let author = msg.auth.user;

        let slug = match &msg.article.title {
            Some(title) => Some(generate_slug(&article.id, &title)),
            None => None,
        };

        let article_change = ArticleChange {
            slug,
            title: msg.article.title,
            description: msg.article.description,
            body: msg.article.body,
        };

        let article = diesel::update(articles::table.find(article.id))
            .set(&article_change)
            .get_result::<Article>(conn)?;

        let tags = match msg.article.tag_list {
            Some(tags) => {
                let inserted_tags = replace_tags(article.id, tags, conn)?;
                inserted_tags
                    .iter()
                    .map(|article_tag| article_tag.tag_name.to_owned())
                    .collect::<Vec<String>>()
            }
            None => select_tags_on_article(article.id, conn)?,
        };

        let favorites_count = get_favorites_count(article.id, conn)?;

        let favorited = get_favorited(article.id, author.id, conn)?;

        Ok(ArticleResponse {
            article: ArticleResponseInner {
                slug: article.slug,
                title: article.title,
                description: article.description,
                body: article.body,
                tag_list: tags,
                created_at: CustomDateTime(article.created_at),
                updated_at: CustomDateTime(article.updated_at),
                favorited,
                favorites_count,
                author: ProfileResponseInner {
                    username: author.username,
                    bio: author.bio,
                    image: author.image,
                    following: false,
                },
            },
        })
    }
}

impl Message for DeleteArticle {
    type Result = Result<()>;
}

impl Handler<DeleteArticle> for DbExecutor {
    type Result = Result<()>;

    fn handle(&mut self, msg: DeleteArticle, _: &mut Self::Context) -> Self::Result {
        use crate::schema::articles;

        let conn = &self.0.get()?;

        let article = articles::table
            .filter(articles::slug.eq(msg.slug))
            .get_result::<Article>(conn)?;

        let author = msg.auth.user;

        if author.id != article.author_id {
            return Err(Error::Forbidden(json!({
                "error": "user is not the author of article in question",
            })));
        }

        delete_tags(article.id, conn)?;

        delete_favorites(article.id, conn)?;

        match diesel::delete(articles::table.filter(articles::id.eq(article.id))).execute(conn) {
            Ok(_) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

impl Message for FavoriteArticle {
    type Result = Result<ArticleResponse>;
}

impl Handler<FavoriteArticle> for DbExecutor {
    type Result = Result<ArticleResponse>;

    fn handle(&mut self, msg: FavoriteArticle, _: &mut Self::Context) -> Self::Result {
        use crate::schema::{articles, favorite_articles, users};

        let conn = &self.0.get()?;

        let (article, author) = articles::table
            .inner_join(users::table)
            .filter(articles::slug.eq(msg.slug))
            .get_result::<(Article, User)>(conn)?;

        diesel::insert_into(favorite_articles::table)
            .values(NewFavoriteArticle {
                user_id: msg.auth.user.id,
                article_id: article.id,
            })
            .execute(conn)?;

        let (favorited, following) =
            get_favorited_and_following(article.id, author.id, msg.auth.user.id, conn)?;

        let favorites_count = get_favorites_count(article.id, conn)?;

        let tags = select_tags_on_article(article.id, conn)?;

        Ok(ArticleResponse {
            article: ArticleResponseInner {
                slug: article.slug,
                title: article.title,
                description: article.description,
                body: article.body,
                tag_list: tags,
                created_at: CustomDateTime(article.created_at),
                updated_at: CustomDateTime(article.updated_at),
                favorited,
                favorites_count,
                author: ProfileResponseInner {
                    username: author.username,
                    bio: author.bio,
                    image: author.image,
                    following,
                },
            },
        })
    }
}

impl Message for UnfavoriteArticle {
    type Result = Result<ArticleResponse>;
}

impl Handler<UnfavoriteArticle> for DbExecutor {
    type Result = Result<ArticleResponse>;

    fn handle(&mut self, msg: UnfavoriteArticle, _: &mut Self::Context) -> Self::Result {
        use crate::schema::{articles, favorite_articles, users};

        let conn = &self.0.get()?;

        let (article, author) = articles::table
            .inner_join(users::table)
            .filter(articles::slug.eq(msg.slug))
            .get_result::<(Article, User)>(conn)?;

        diesel::delete(favorite_articles::table)
            .filter(favorite_articles::user_id.eq(msg.auth.user.id))
            .filter(favorite_articles::article_id.eq(article.id))
            .execute(conn)?;

        let (favorited, following) =
            get_favorited_and_following(article.id, author.id, msg.auth.user.id, conn)?;

        let favorites_count = get_favorites_count(article.id, conn)?;

        let tags = select_tags_on_article(article.id, conn)?;

        Ok(ArticleResponse {
            article: ArticleResponseInner {
                slug: article.slug,
                title: article.title,
                description: article.description,
                body: article.body,
                tag_list: tags,
                created_at: CustomDateTime(article.created_at),
                updated_at: CustomDateTime(article.updated_at),
                favorited,
                favorites_count,
                author: ProfileResponseInner {
                    username: author.username,
                    bio: author.bio,
                    image: author.image,
                    following,
                },
            },
        })
    }
}

impl Message for GetArticles {
    type Result = Result<ArticleListResponse>;
}

impl Handler<GetArticles> for DbExecutor {
    type Result = Result<ArticleListResponse>;

    fn handle(&mut self, msg: GetArticles, _: &mut Self::Context) -> Self::Result {
        use crate::schema::articles;

        let conn = &self.0.get()?;

        unimplemented!()
    }
}

impl Message for GetFeed {
    type Result = Result<ArticleListResponse>;
}

impl Handler<GetFeed> for DbExecutor {
    type Result = Result<ArticleListResponse>;

    fn handle(&mut self, msg: GetFeed, _: &mut Self::Context) -> Self::Result {
        use crate::schema::articles;

        let conn = &self.0.get()?;

        unimplemented!()
    }
}

fn generate_slug(uuid: &Uuid, title: &str) -> String {
    format!("{}-{}", to_blob(uuid), slugify(title))
}

fn add_tag<T>(article_id: Uuid, tag_name: T, conn: &PooledConn) -> Result<ArticleTag>
where
    T: ToString,
{
    use crate::schema::article_tags;

    diesel::insert_into(article_tags::table)
        .values(NewArticleTag {
            article_id,
            tag_name: tag_name.to_string(),
        })
        .on_conflict((article_tags::article_id, article_tags::tag_name))
        .do_nothing()
        .get_result::<ArticleTag>(conn)
        .map_err(std::convert::Into::into) // <- clippy doesn't like it when I write this as |e| e.into() so...
}

fn delete_tags(article_id: Uuid, conn: &PooledConn) -> Result<()> {
    use crate::schema::article_tags;

    diesel::delete(article_tags::table.filter(article_tags::article_id.eq(article_id)))
        .execute(conn)?;
    Ok(())
}

fn delete_favorites(article_id: Uuid, conn: &PooledConn) -> Result<()> {
    use crate::schema::favorite_articles;

    diesel::delete(favorite_articles::table.filter(favorite_articles::article_id.eq(article_id)))
        .execute(conn)?;
    Ok(())
}

fn replace_tags<I>(article_id: Uuid, tags: I, conn: &PooledConn) -> Result<Vec<ArticleTag>>
where
    I: IntoIterator<Item = String>,
{
    delete_tags(article_id, conn)?;

    // this may look confusing but collect can convert to this
    // https://doc.rust-lang.org/std/iter/trait.Iterator.html#method.collect
    tags.into_iter()
        .map(|tag_name| add_tag(article_id, &tag_name.to_string(), conn))
        .collect::<Result<Vec<ArticleTag>>>()
}

fn get_favorites_count(article_id: Uuid, conn: &PooledConn) -> Result<usize> {
    use crate::schema::favorite_articles;

    let favorites_count = favorite_articles::table
        .filter(favorite_articles::article_id.eq(article_id))
        .count()
        .get_result::<i64>(conn)?;
    Ok(favorites_count as usize)
}

fn get_favorited(article_id: Uuid, user_id: Uuid, conn: &PooledConn) -> Result<bool> {
    use crate::schema::{favorite_articles, users};

    let (_, favorite_id) = users::table
        .left_join(
            favorite_articles::table.on(favorite_articles::user_id
                .eq(users::id)
                .and(favorite_articles::article_id.eq(article_id))),
        )
        .filter(users::id.eq(user_id))
        .select((users::id, favorite_articles::user_id.nullable()))
        .get_result::<(Uuid, Option<Uuid>)>(conn)?;

    Ok(favorite_id.is_some())
}

fn get_favorited_and_following(
    article_id: Uuid,
    author_id: Uuid,
    user_id: Uuid,
    conn: &PooledConn,
) -> Result<(bool, bool)> {
    use crate::schema::{favorite_articles, followers, users};

    // joins don't look pretty in Diesel, I know
    let (_, favorite_id, follow_id) = users::table
        .left_join(
            favorite_articles::table.on(favorite_articles::user_id
                .eq(users::id)
                .and(favorite_articles::article_id.eq(article_id))),
        )
        .left_join(
            followers::table.on(followers::follower_id
                .eq(users::id)
                .and(followers::user_id.eq(author_id))),
        )
        .filter(users::id.eq(user_id))
        .select((
            users::id,
            favorite_articles::user_id.nullable(),
            followers::follower_id.nullable(),
        ))
        .get_result::<(Uuid, Option<Uuid>, Option<Uuid>)>(conn)?;

    Ok((favorite_id.is_some(), follow_id.is_some()))
}

fn select_tags_on_article(article_id: Uuid, conn: &PooledConn) -> Result<Vec<String>> {
    use crate::schema::article_tags;

    let tags = article_tags::table
        .filter(article_tags::article_id.eq(article_id))
        .select(article_tags::tag_name)
        .load(conn)?;

    Ok(tags)
}
